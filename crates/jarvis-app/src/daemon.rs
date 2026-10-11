//! Демон: трей в главном потоке, ядро (автомат состояний, команды, звук
//! ответа) — в потоке `jarvis-core`, слух — в потоке `jarvis-listen`,
//! каждое действие — в своём коротком потоке `jarvis-action`.
//!
//! ```text
//!  jarvis-listen ──PipelineEvent──▶ jarvis-core ──Indicator──▶ главный поток (трей)
//!        ▲                          │  ▲    │                       │
//!        └─ микрофон / распознавание┘  │    └─▶ jarvis-action ─┐    │
//!                                      └──── результат ────────┘    │
//!                                      └────── Пауза / Выход ───────┘
//! ```

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_actions::{ActionError, Outcome, Sound, System};
use jarvis_app::resolver::{CommandResolver, EarlyDecider, Resolution, hotword_phrases};
use jarvis_app::system::PlatformSystem;
use jarvis_audio::asr::Hotwords;
use jarvis_audio::pack::VoicePack;
use jarvis_audio::pipeline::{MuteGate, Pipeline, PipelineEvent, PipelineSettings, WakeEvent};
use jarvis_audio::player::Player;
use jarvis_audio::stt::Transcript;
use jarvis_core::Config;
use jarvis_core::commands::{Command, Registry, ReplyChoice};
use jarvis_core::config::resolve;
use jarvis_core::state::{Effect, Event, Reply, State, Timings, transition};
use jarvis_platform::Waker;

use crate::hearing;
use crate::tray::{Indicator, MenuAction, Tray};

pub const INSTANCE_NAME: &str = "JarvisVoiceAssistant";
/// Как часто ядро проверяет таймауты и раннее решение.
const CORE_TICK: Duration = Duration::from_millis(50);
/// Страховочный такт цикла сообщений (см. jarvis_platform::run_message_loop).
const UI_TICK: Duration = Duration::from_millis(250);
/// После своей фразы ещё немного не слушаем: эхо комнаты.
const ECHO_TAIL: Duration = Duration::from_millis(150);
/// Сколько ждём действие, прежде чем сказать «не удалось» (этапа 2 действия
/// мгновенные; скрипты со своим таймаутом — этап 4).
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

enum CoreMsg {
    Pipeline(PipelineEvent),
    ActionDone {
        generation: u64,
        result: Result<Outcome, ActionError>,
    },
    TogglePause,
    Shutdown,
}

/// Всё, что ядру нужно для старта.
struct CoreParams {
    settings: PipelineSettings,
    registry: Registry,
    resolver: CommandResolver,
    pack: VoicePack,
    config: Config,
    ui: Sender<Indicator>,
    waker: Waker,
}

pub fn run(root: &Path, config: Config) -> anyhow::Result<()> {
    let _instance = jarvis_platform::acquire_single_instance(INSTANCE_NAME)?
        .context("Джарвис уже запущен — его значок в трее")?;

    let hearing = hearing::prepare(root, &config)?;
    let pack_dir = resolve(root, &config.paths.sounds).join(&config.voice.pack);
    let pack = VoicePack::load(&pack_dir).context("звуковой пакет не загружен")?;
    let registry = load_commands(root, &config);

    let hotwords = (config.listen.hotwords_score > 0.0).then(|| Hotwords {
        phrases: hotword_phrases(&registry, &config.wake.words),
        score: config.listen.hotwords_score,
    });
    let resolver = CommandResolver::new(&registry, hearing.matcher.clone(), &config.nlu);

    let waker = jarvis_platform::current_waker();
    jarvis_platform::install_ctrl_handler(waker)?;
    // `jarvis --stop` из другого процесса.
    let stop = jarvis_platform::create_stop_signal(INSTANCE_NAME)?;
    std::thread::Builder::new()
        .name("jarvis-stop".into())
        .spawn(move || {
            if stop.wait().is_ok() {
                tracing::info!("получена команда остановки (jarvis --stop)");
                waker.quit();
            }
        })
        .context("не удалось запустить поток остановки")?;
    let mut tray = Tray::new()?;

    let (core_tx, core_rx) = mpsc::channel::<CoreMsg>();
    let (ui_tx, ui_rx) = mpsc::channel::<Indicator>();
    let params = CoreParams {
        settings: PipelineSettings {
            device: config.audio.input_device_name().map(str::to_owned),
            models: hearing.models,
            matcher: hearing.matcher,
            voiceprint: hearing.voiceprint,
            end_silence: config.listen.end_silence_ms as f32 / 1000.0,
            hotwords,
        },
        registry,
        resolver,
        pack,
        config,
        ui: ui_tx,
        waker,
    };

    let core_events = core_tx.clone();
    let core = std::thread::Builder::new()
        .name("jarvis-core".into())
        .spawn(move || match Core::start(params, core_events) {
            Ok(mut core) => core.run(core_rx),
            Err(e) => {
                tracing::error!(error = %jarvis_core::error_chain(e.as_ref()), "ядро не запустилось");
                waker.quit();
            }
        })
        .context("не удалось запустить поток ядра")?;

    tracing::info!("Джарвис в трее; выход — пункт «Выход» или Ctrl+C");
    jarvis_platform::run_message_loop(UI_TICK, || {
        while let Ok(indicator) = ui_rx.try_recv() {
            tray.show(indicator);
        }
        match tray.poll_menu() {
            Some(MenuAction::TogglePause) => {
                let _ = core_tx.send(CoreMsg::TogglePause);
                ControlFlow::Continue(())
            }
            Some(MenuAction::Quit) => ControlFlow::Break(()),
            None => ControlFlow::Continue(()),
        }
    })?;

    tracing::info!("завершение работы");
    let _ = core_tx.send(CoreMsg::Shutdown);
    if core.join().is_err() {
        tracing::error!("поток ядра завершился паникой");
    }
    drop(tray);
    tracing::info!("Джарвис остановлен");
    jarvis_platform::notify_shutdown_complete();
    Ok(())
}

/// Загружает пакеты команд и пишет в лог всё, что с ними не так.
pub fn load_commands(root: &Path, config: &Config) -> Registry {
    let dir = resolve(root, &config.paths.commands);
    let registry = Registry::load(&dir);
    for problem in &registry.errors {
        tracing::error!("пропущено: {problem}");
    }
    for warning in &registry.warnings {
        tracing::warn!("{warning}");
    }
    tracing::info!(
        commands = registry.commands.len(),
        errors = registry.errors.len(),
        "команды загружены"
    );
    registry
}

/// Ядро: автомат состояний и исполнение его эффектов.
struct Core {
    state: State,
    timings: Timings,
    pipeline: Pipeline,
    player: Player,
    pack: VoicePack,
    gate: Arc<MuteGate>,
    registry: Registry,
    resolver: CommandResolver,
    early: EarlyDecider,
    system: Arc<dyn System>,
    dry_run: bool,
    events: Sender<CoreMsg>,
    /// Номер текущего действия: поздний результат старого игнорируется.
    generation: u64,
    executing: Option<Running>,
    /// Когда услышана команда, которую автомат сейчас отправит на выполнение.
    pending_heard_at: Option<Instant>,
    ui: Sender<Indicator>,
    waker: Waker,
    /// Слух сообщил о проблеме — показываем её, пока не наладится.
    problem: Option<String>,
    ready: bool,
}

struct Running {
    command: Command,
    started: Instant,
    /// Когда закончилась фраза пользователя (для замера «от фразы до действия»).
    heard_at: Instant,
}

impl Core {
    fn start(p: CoreParams, events: Sender<CoreMsg>) -> anyhow::Result<Self> {
        // Player создаётся здесь: поток вывода звука нельзя передавать между потоками.
        let player =
            Player::new(&p.pack, p.config.voice.volume).context("вывод звука не открылся")?;
        let gate = Arc::new(MuteGate::default());
        let pipeline_events = events.clone();
        let pipeline = Pipeline::spawn(
            p.settings,
            Box::new(move |event| {
                let _ = pipeline_events.send(CoreMsg::Pipeline(event));
            }),
            Arc::clone(&gate),
        )
        .context("не удалось запустить поток слуха")?;
        let nlu = &p.config.nlu;
        Ok(Self {
            state: State::Idle,
            timings: Timings {
                listen: Duration::from_secs(u64::from(p.config.wake.listen_secs)),
                follow_up: Duration::from_secs(u64::from(p.config.session.follow_up_secs)),
            },
            pipeline,
            player,
            pack: p.pack,
            gate,
            registry: p.registry,
            resolver: p.resolver,
            early: EarlyDecider::new(
                nlu.early_score,
                Duration::from_millis(u64::from(nlu.early_stable_ms)),
            ),
            system: Arc::new(PlatformSystem),
            dry_run: p.config.general.dry_run,
            events,
            generation: 0,
            executing: None,
            pending_heard_at: None,
            ui: p.ui,
            waker: p.waker,
            problem: None,
            ready: false,
        })
    }

    fn run(&mut self, rx: Receiver<CoreMsg>) {
        loop {
            match rx.recv_timeout(CORE_TICK) {
                Ok(CoreMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(CoreMsg::TogglePause) => self.apply(Event::TogglePause),
                Ok(CoreMsg::Pipeline(event)) => self.on_pipeline(event),
                Ok(CoreMsg::ActionDone { generation, result }) => {
                    self.on_action_done(generation, result)
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.on_tick();
        }
        self.player.stop();
        // Pipeline при удалении останавливает поток и освобождает микрофон.
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        if self.state.accepts_commands()
            && let Some((utterance, id)) = self.early.poll(now)
        {
            tracing::debug!(utterance, %id, "раннее решение по частичному тексту");
            self.run_command(id, now);
        }
        if let Some(running) = &self.executing
            && running.started.elapsed() > ACTION_TIMEOUT
        {
            tracing::error!(command = %running.command.id, "действие не ответило за {ACTION_TIMEOUT:?}");
            self.executing = None;
            self.generation += 1;
            self.apply(Event::ActionFinished {
                reply: Some(Reply::Failed),
            });
        }
        self.apply(Event::Tick);
    }

    fn on_pipeline(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::Ready { load_time } => {
                tracing::info!(
                    ms = load_time.as_millis() as u64,
                    "модели загружены и прогреты"
                );
                self.ready = true;
                self.refresh_ui();
            }
            PipelineEvent::Listening {
                device,
                sample_rate,
            } => {
                tracing::info!(%device, sample_rate, "слушаю");
                self.problem = None;
                self.refresh_ui();
            }
            PipelineEvent::Wake(wake) => self.on_wake(wake),
            PipelineEvent::Transcript(t) => self.on_transcript(t),
            PipelineEvent::Rejected {
                heard,
                similarity,
                threshold,
            } => {
                tracing::info!(%heard, similarity, threshold, "«Джарвис» сказал не ваш голос — игнорирую");
            }
            PipelineEvent::CaptureLost(why) => {
                tracing::warn!(%why, "микрофон недоступен, пробую снова");
                self.problem = Some(why);
                self.refresh_ui();
            }
            PipelineEvent::Fatal(why) => {
                tracing::error!(%why, "слух не работает");
                self.problem = Some(why);
                self.refresh_ui();
            }
        }
    }

    fn on_wake(&mut self, wake: WakeEvent) {
        let answering = matches!(self.state, State::Idle | State::FollowUp { .. });
        self.apply(Event::WakeWord);
        if answering {
            tracing::info!(
                heard = %wake.heard,
                voice = ?wake.similarity,
                verify_ms = wake.verify_time.as_millis() as u64,
                reply_ms = wake.detected_at.elapsed().as_millis() as u64,
                "«Джарвис» — отвечаю"
            );
        }
    }

    fn on_transcript(&mut self, t: Transcript) {
        if !self.state.accepts_commands() {
            self.early.clear();
            return;
        }
        let now = Instant::now();
        let resolution = self.resolver.resolve(&t.text);
        if !t.is_final {
            tracing::trace!(text = %t.text, ?resolution, "частичный текст");
            let instant = self.resolver.instant(&t.text, &resolution);
            if let Some(id) = self
                .early
                .on_partial(t.utterance, &t.text, &resolution, instant, now)
            {
                tracing::debug!(text = %t.text, %id, "решение по началу фразы");
                self.run_command(id, now);
            }
            return;
        }
        if !self.early.on_final(t.utterance) {
            return; // по этой фразе команда уже выполнена раньше
        }
        tracing::info!(text = %t.text, ?resolution, "фраза");
        match resolution {
            Resolution::Command { id, .. } => self.run_command(id, now),
            Resolution::Ambiguous { first, second } => {
                // Уточняющий вопрос голосом («X или Y?») — с синтезом речи (этап 5).
                tracing::info!(%first, %second, "неоднозначно — переспрашиваю");
                self.apply(Event::NotUnderstood);
            }
            Resolution::NotUnderstood { best } => {
                tracing::info!(?best, "не понял");
                self.apply(Event::NotUnderstood);
            }
            Resolution::Empty => {}
        }
    }

    fn run_command(&mut self, id: String, heard_at: Instant) {
        self.early.clear();
        if self.registry.get(&id).is_none() {
            tracing::error!(%id, "команды нет в реестре");
            return;
        }
        self.pending_heard_at = Some(heard_at);
        self.apply(Event::Command(id));
    }

    fn execute(&mut self, id: &str) {
        let Some(command) = self.registry.get(id).cloned() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let heard_at = self.pending_heard_at.take().unwrap_or_else(Instant::now);
        tracing::info!(
            command = %command.id,
            action = %command.action.summary(),
            dry_run = self.dry_run,
            "выполняю"
        );
        self.executing = Some(Running {
            command: command.clone(),
            started: Instant::now(),
            heard_at,
        });

        if self.dry_run {
            let _ = self.events.send(CoreMsg::ActionDone {
                generation,
                result: Ok(Outcome::Done),
            });
            return;
        }
        let system = Arc::clone(&self.system);
        let events = self.events.clone();
        let spawned = std::thread::Builder::new()
            .name("jarvis-action".into())
            .spawn(move || {
                // Паника в действии не должна ронять ядро (раздел 2.5 ТЗ).
                let result = catch_unwind(AssertUnwindSafe(|| {
                    jarvis_actions::execute(&command.action, &command.dir, system.as_ref())
                }))
                .unwrap_or_else(|_| {
                    Err(ActionError::Invalid("действие завершилось паникой".into()))
                });
                let _ = events.send(CoreMsg::ActionDone { generation, result });
            });
        if let Err(e) = spawned {
            let _ = self.events.send(CoreMsg::ActionDone {
                generation,
                result: Err(ActionError::Invalid(format!(
                    "не удалось запустить поток: {e}"
                ))),
            });
        }
    }

    fn on_action_done(&mut self, generation: u64, result: Result<Outcome, ActionError>) {
        if generation != self.generation {
            return; // опоздавший результат уже отменённого действия
        }
        let Some(running) = self.executing.take() else {
            return;
        };
        let elapsed = running.started.elapsed();
        let reply = match result {
            Ok(Outcome::Done) => {
                tracing::info!(
                    command = %running.command.id,
                    action_ms = elapsed.as_millis() as u64,
                    since_heard_ms = running.heard_at.elapsed().as_millis() as u64,
                    "готово"
                );
                match running.command.reply {
                    ReplyChoice::Reply(r) => Some(r),
                    ReplyChoice::Silent => None,
                }
            }
            Ok(Outcome::Play(sound)) => {
                self.play_sound(&sound);
                None
            }
            Err(e) => {
                tracing::error!(command = %running.command.id, error = %e, "не удалось");
                Some(Reply::Failed)
            }
        };
        self.apply(Event::ActionFinished { reply });
    }

    fn play_sound(&mut self, sound: &Sound) {
        let duration = match sound {
            Sound::Reply(key) => Reply::ALL
                .into_iter()
                .find(|r| r.key() == key)
                .and_then(|r| self.player.play(r)),
            Sound::File(path) => {
                match jarvis_audio::wav::read(path).and_then(|clip| self.player.play_clip(&clip)) {
                    Ok(d) => Some(d),
                    Err(e) => {
                        tracing::error!(error = %jarvis_core::error_chain(&e), "не удалось проиграть звук");
                        None
                    }
                }
            }
        };
        if let Some(d) = duration {
            self.gate.mute_for(d + ECHO_TAIL);
        }
    }

    fn apply(&mut self, event: Event) {
        let before = self.state;
        let tr = transition(self.state, event.clone(), Instant::now(), self.timings);
        self.state = tr.next;
        for effect in tr.effects {
            match effect {
                Effect::PlayReply(reply) => {
                    if self.pack.variants(reply).is_empty() {
                        continue;
                    }
                    if let Some(duration) = self.player.play(reply) {
                        self.gate.mute_for(duration + ECHO_TAIL);
                    }
                }
                Effect::SetListening(on) => self.pipeline.set_listening(on),
                Effect::SetTranscribing(on) => {
                    self.early.clear();
                    self.pipeline.set_transcribing(on);
                }
                Effect::Execute(id) => self.execute(&id),
            }
        }
        if std::mem::discriminant(&before) != std::mem::discriminant(&self.state) {
            tracing::debug!(from = ?before, to = ?self.state, ?event, "переход");
            self.refresh_ui();
        }
    }

    fn refresh_ui(&self) {
        let indicator = match (&self.state, &self.problem, self.ready) {
            (State::Paused, _, _) => Indicator::Paused,
            (_, Some(why), _) => Indicator::Problem(why.clone()),
            (_, None, false) => Indicator::Starting,
            (State::Idle, None, true) => Indicator::Idle,
            (State::Listening { .. } | State::Executing | State::FollowUp { .. }, None, true) => {
                Indicator::Listening
            }
        };
        if self.ui.send(indicator).is_ok() {
            self.waker.wake();
        }
    }
}
