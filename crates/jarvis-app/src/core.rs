//! Ядро: автомат состояний, команды, разрешения, журнал, звук ответа.
//! Работает в потоке `jarvis-core`; события приходят через канал.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use std::path::PathBuf;

use anyhow::Context as _;
use jarvis_actions::{ActionError, CancelToken, Outcome, Sound, System};
use jarvis_app::resolver::{
    BUILTIN_FORGET_PERMISSIONS, BUILTIN_RELOAD_COMMANDS, BUILTIN_STOP, CommandResolver,
    EarlyDecider, Resolution, hotword_phrases, is_builtin, spoken_phrase,
};
use jarvis_app::speech::{self, Speech, SpeechEvent};
use jarvis_app::system::PlatformSystem;
use jarvis_audio::pack::{DayPart, VoicePack};
use jarvis_audio::pipeline::{MuteGate, Pipeline, PipelineEvent, PipelineSettings, WakeEvent};
use jarvis_audio::player::Player;
use jarvis_audio::stt::Transcript;
use jarvis_audio::tts::VoiceModel;
use jarvis_audio::wake::WakeMatcher;
use jarvis_core::Config;
use jarvis_core::commands::{ActionSpec, Command, Registry, ReplyChoice};
use jarvis_core::journal::{self, ActionJournal, AnswerSource, Entry, PermissionNote};
use jarvis_core::permissions::{Answer, PermissionStore, Verdict};
use jarvis_core::slots::SlotValues;
use jarvis_core::state::{Effect, Event, Reply, State, Timings, transition};
use jarvis_nlu::answers::{PermissionReply, parse_reply};
use jarvis_platform::{DialogButton, DialogHandle, DialogSpec, Waker};

use crate::tray::Indicator;

/// Как часто ядро проверяет таймауты и раннее решение.
const CORE_TICK: Duration = Duration::from_millis(50);
/// После своей фразы ещё немного не слушаем: эхо комнаты.
const ECHO_TAIL: Duration = Duration::from_millis(150);
/// Сколько ждём действие, прежде чем сказать «не удалось».
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём ответа на вопрос-разрешение (раздел 2.1 ТЗ).
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

const BUTTON_YES: i32 = 100;
const BUTTON_ALWAYS: i32 = 101;
const BUTTON_NO: i32 = 102;
const BUTTON_NEVER: i32 = 103;

pub enum CoreMsg {
    Pipeline(PipelineEvent),
    ActionDone {
        generation: u64,
        result: Result<Outcome, ActionError>,
    },
    /// Нажата кнопка в окне-вопросе (`None` — окно закрыли).
    DialogAnswer {
        generation: u64,
        button: Option<i32>,
    },
    /// Звук из шага цепочки (`say` внутри `sequence`).
    PlaySound(Sound),
    /// Поток синтеза речи.
    Speech(SpeechEvent),
    TogglePause,
    EmergencyStop,
    ForgetPermissions,
    ReloadCommands,
    Shutdown,
}

/// Всё, что ядру нужно для старта.
pub struct CoreParams {
    pub settings: PipelineSettings,
    pub registry: Registry,
    pub resolver: CommandResolver,
    pub pack: VoicePack,
    pub permissions: PermissionStore,
    pub journal: Option<ActionJournal>,
    pub config: Config,
    /// Для перезагрузки команд: папка команд, слова активации.
    pub commands_dir: PathBuf,
    pub wake_matcher: WakeMatcher,
    /// Голос синтеза; `None` — только звуковой пакет.
    pub voice: Option<VoiceSetup>,
    /// Запущены от администратора — предупредить голосом (раздел 2.5 ТЗ).
    pub elevated: bool,
    pub ui: Sender<Indicator>,
    pub waker: Waker,
}

pub struct VoiceSetup {
    pub name: String,
    pub model: VoiceModel,
    pub speed: f32,
}

/// Зачем синтезируется фраза — от этого зависит, нужна ли она ещё, когда
/// будет готова, и чем её заменить, если синтез не удался.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Вопрос-разрешение для окна с этим номером.
    Ask(u64),
    /// «Вы имели в виду X или Y?»
    Clarify,
    /// Предупреждение без ожидания ответа.
    Notice,
}

/// Что услышали — для журнала и подстановки слотов.
#[derive(Debug, Clone)]
struct Heard {
    text: String,
    score: f32,
    slots: SlotValues,
    at: Instant,
}

/// Команда ждёт ответа на вопрос-разрешение.
struct Pending {
    id: String,
    heard: Heard,
    offer_always: bool,
}

/// Как команда получила право выполниться — переходит в `Running`.
struct Approval {
    heard: Heard,
    note: PermissionNote,
    answered_by: Option<AnswerSource>,
}

struct Running {
    id: String,
    /// `None` — встроенная команда.
    command: Option<Command>,
    started: Instant,
    /// Дольше — действие считается зависшим.
    deadline: Duration,
    approval: Approval,
}

/// Сколько действие может длиться законно (плюс запас) — для сторожа ядра.
fn expected_duration(action: &ActionSpec) -> Duration {
    use jarvis_core::commands::DEFAULT_SCRIPT_TIMEOUT_SEC;
    match action {
        ActionSpec::RunScript { timeout_sec, .. } => {
            Duration::from_secs(timeout_sec.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SEC) + 5)
        }
        ActionSpec::Wait { ms } => Duration::from_millis(*ms) + ACTION_TIMEOUT,
        ActionSpec::KeyPress {
            hold_ms,
            repeat,
            interval_ms,
            ..
        } => {
            let per = hold_ms.unwrap_or(30) + interval_ms.unwrap_or(50);
            Duration::from_millis(per * u64::from(repeat.unwrap_or(1))) + ACTION_TIMEOUT
        }
        ActionSpec::Hotkey { hold_ms, .. } => {
            Duration::from_millis(hold_ms.unwrap_or(30)) + ACTION_TIMEOUT
        }
        ActionSpec::Sequence { steps } => steps.iter().map(expected_duration).sum(),
        _ => ACTION_TIMEOUT,
    }
}

pub struct Core {
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
    permissions: PermissionStore,
    journal: Option<ActionJournal>,
    config: Config,
    commands_dir: PathBuf,
    wake_matcher: WakeMatcher,
    /// Отмена текущего действия (аварийная остановка).
    cancel: Option<CancelToken>,
    events: Sender<CoreMsg>,
    /// Номер текущего действия: поздний результат старого игнорируется.
    generation: u64,
    executing: Option<Running>,
    /// Одобрение для команды, которую автомат сейчас отправит на выполнение.
    approval: Option<Approval>,
    pending: Option<Pending>,
    dialog: Option<DialogHandle>,
    /// Номер окна-вопроса: ответ из закрытого окна игнорируется.
    ask_generation: u64,
    ui: Sender<Indicator>,
    waker: Waker,
    /// Слух сообщил о проблеме — показываем её, пока не наладится.
    problem: Option<String>,
    ready: bool,
    speech: Option<Speech>,
    /// Голос загружен — вопрос звучит синтезом, а не фразой из пакета.
    speech_ready: bool,
    /// Номер последнего запроса синтеза: старые результаты не нужны.
    speech_id: u64,
    speaking: Option<Purpose>,
    /// Вопрос-разрешение звучит до этого момента (ответ кнопкой его обрывает).
    asking_aloud_until: Option<Instant>,
    /// Ответ «не понял» заменён уточняющим вопросом.
    clarifying: bool,
    /// Сколько Джарвис проговорил с прошлого `apply` (см. `flush_spoke`).
    spoke_pending: Duration,
    /// Приветствие при запуске уже прозвучало (слух может перезапуститься).
    greeted: bool,
    warn_elevated: bool,
}

impl Core {
    pub fn start(p: CoreParams, events: Sender<CoreMsg>) -> anyhow::Result<Self> {
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
        let speech = p.voice.and_then(|voice| {
            let speech_events = events.clone();
            Speech::spawn(
                voice.name,
                voice.model,
                voice.speed,
                Box::new(move |event| {
                    let _ = speech_events.send(CoreMsg::Speech(event));
                }),
            )
            .inspect_err(|e| tracing::error!(error = %e, "не удалось запустить поток синтеза"))
            .ok()
        });
        let nlu = &p.config.nlu;
        Ok(Self {
            state: State::Idle,
            timings: Timings {
                listen: Duration::from_secs(u64::from(p.config.wake.listen_secs)),
                follow_up: Duration::from_secs(u64::from(p.config.session.follow_up_secs)),
                ask: ASK_TIMEOUT,
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
            permissions: p.permissions,
            journal: p.journal,
            config: p.config.clone(),
            commands_dir: p.commands_dir,
            wake_matcher: p.wake_matcher,
            cancel: None,
            events,
            generation: 0,
            executing: None,
            approval: None,
            pending: None,
            dialog: None,
            ask_generation: 0,
            ui: p.ui,
            waker: p.waker,
            problem: None,
            ready: false,
            speech,
            speech_ready: false,
            speech_id: 0,
            speaking: None,
            asking_aloud_until: None,
            clarifying: false,
            spoke_pending: Duration::ZERO,
            greeted: false,
            warn_elevated: p.elevated,
        })
    }

    pub fn run(&mut self, rx: Receiver<CoreMsg>) {
        loop {
            match rx.recv_timeout(CORE_TICK) {
                Ok(CoreMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(CoreMsg::TogglePause) => self.apply(Event::TogglePause),
                Ok(CoreMsg::EmergencyStop) => self.emergency_stop("горячая клавиша"),
                Ok(CoreMsg::ForgetPermissions) => self.forget_permissions("трей"),
                Ok(CoreMsg::ReloadCommands) => {
                    let ok = self.reload_commands("трей");
                    self.play(if ok { Reply::Done } else { Reply::Failed });
                }
                Ok(CoreMsg::PlaySound(sound)) => self.play_sound(&sound),
                Ok(CoreMsg::Speech(event)) => self.on_speech(event),
                Ok(CoreMsg::Pipeline(event)) => self.on_pipeline(event),
                Ok(CoreMsg::ActionDone { generation, result }) => {
                    self.on_action_done(generation, result)
                }
                Ok(CoreMsg::DialogAnswer { generation, button }) => {
                    self.on_dialog(generation, button)
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.on_tick();
        }
        if let Some(mut dialog) = self.dialog.take() {
            dialog.close();
        }
        self.player.stop();
        // Pipeline при удалении останавливает поток и освобождает микрофон.
    }

    fn on_tick(&mut self) {
        // Сменилось устройство вывода — переоткрыть, не дожидаясь ответа.
        self.player.maintain();
        let now = Instant::now();
        if self.state.accepts_commands()
            && let Some((utterance, id)) = self.early.poll(now)
        {
            tracing::debug!(utterance, %id, "раннее решение по частичному тексту");
            let score = self.early_score();
            self.command(id, String::new(), score, SlotValues::new(), now);
        }
        if let Some(running) = &self.executing
            && running.started.elapsed() > running.deadline
        {
            tracing::error!(command = %running.id, deadline = ?running.deadline, "действие не ответило вовремя");
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
            if let Some(running) = self.executing.take() {
                self.write_journal(
                    &running.approval,
                    &running.id,
                    "failed",
                    Some("тайм-аут".into()),
                );
            }
            self.generation += 1;
            self.apply(Event::ActionFinished {
                reply: Some(Reply::Failed),
            });
        }
        self.apply(Event::Tick);
    }

    fn early_score(&self) -> f32 {
        1.0
    }

    // ---------- события слуха ----------

    fn on_pipeline(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::Ready { load_time } => {
                tracing::info!(
                    ms = load_time.as_millis() as u64,
                    "модели загружены и прогреты"
                );
                self.ready = true;
                self.refresh_ui();
                if !std::mem::replace(&mut self.greeted, true) && self.config.general.greet_on_start
                {
                    self.greet();
                }
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
        let answering = matches!(
            self.state,
            State::Idle | State::Listening { .. } | State::FollowUp { .. }
        );
        self.apply(Event::WakeWord);
        if answering {
            // Пока человек договаривает команду, голос успеет загрузиться —
            // вдруг понадобится вопрос-разрешение с подробностями.
            if let Some(speech) = &self.speech
                && self.config.voice.spoken_details
            {
                speech.prepare();
            }
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
        if matches!(self.state, State::Idle | State::Paused) {
            self.early.clear();
            return;
        }
        let now = Instant::now();
        let resolution = self.resolver.resolve(&t.text);

        // «Стоп» — в любом активном состоянии, даже во время вопроса и действия.
        if matches!(&resolution, Resolution::Command { id, .. } if id == BUILTIN_STOP) {
            self.emergency_stop("голос");
            return;
        }

        if matches!(self.state, State::AskingPermission { .. }) {
            if let Some(reply) = parse_reply(&t.text) {
                tracing::info!(text = %t.text, ?reply, "ответ голосом");
                let offer_always = self.pending.as_ref().is_some_and(|p| p.offer_always);
                let answer = match reply {
                    PermissionReply::Yes => Answer::Once,
                    PermissionReply::Always if offer_always => Answer::Always,
                    PermissionReply::Always => Answer::Once,
                    PermissionReply::No => Answer::No,
                    PermissionReply::Never => Answer::Never,
                };
                self.answer(answer, AnswerSource::Voice);
            }
            return;
        }

        if !self.state.accepts_commands() {
            return;
        }
        if !t.is_final {
            tracing::trace!(text = %t.text, ?resolution, "частичный текст");
            if !t.text.trim().is_empty() {
                self.apply(Event::UserSpeaking);
            }
            let instant = self.resolver.instant(&t.text, &resolution);
            let score = match &resolution {
                Resolution::Command { score, .. } => *score,
                _ => 1.0,
            };
            if let Some(id) = self
                .early
                .on_partial(t.utterance, &t.text, &resolution, instant, now)
            {
                tracing::debug!(text = %t.text, %id, "решение по началу фразы");
                // Решение по началу фразы бывает только у команд без слотов.
                self.command(id, t.text, score, SlotValues::new(), now);
            }
            return;
        }
        if !self.early.on_final(t.utterance) {
            return; // по этой фразе команда уже выполнена раньше
        }
        tracing::info!(text = %t.text, ?resolution, "фраза");
        match resolution {
            Resolution::Command { id, score, slots } => self.command(id, t.text, score, slots, now),
            Resolution::Ambiguous { first, second } => {
                tracing::info!(%first, %second, "неоднозначно — переспрашиваю");
                // Уточняющий вопрос — только после «Джарвис»: в FOLLOW_UP
                // непонятная фраза — скорее посторонний разговор.
                if matches!(self.state, State::Listening { .. }) {
                    let text = speech::clarify_text(
                        &spoken_phrase(&self.registry, &first),
                        &spoken_phrase(&self.registry, &second),
                    );
                    self.clarifying = self.speak(text, Purpose::Clarify);
                }
                self.apply(Event::NotUnderstood);
                self.clarifying = false;
            }
            Resolution::NotUnderstood { best } => {
                tracing::info!(?best, "не понял");
                self.apply(Event::NotUnderstood);
            }
            Resolution::Empty => {}
        }
    }

    // ---------- команды и разрешения ----------

    fn command(&mut self, id: String, text: String, score: f32, slots: SlotValues, at: Instant) {
        self.early.clear();
        let heard = Heard {
            text,
            score,
            slots,
            at,
        };

        if id == BUILTIN_STOP {
            self.emergency_stop("голос");
            return;
        }
        if id == BUILTIN_RELOAD_COMMANDS {
            // Безвредная встроенная команда — без вопроса.
            let ok = self.reload_commands("голос");
            let approval = Approval {
                heard,
                note: PermissionNote::Builtin,
                answered_by: None,
            };
            self.write_journal(&approval, &id, if ok { "ok" } else { "failed" }, None);
            self.play(if ok { Reply::Done } else { Reply::Failed });
            return;
        }
        if id == BUILTIN_FORGET_PERMISSIONS {
            // Встроенная, но необратимая — с подтверждением (раздел 2.2).
            self.pending = Some(Pending {
                id: id.clone(),
                heard,
                offer_always: false,
            });
            self.apply(Event::NeedPermission(id));
            return;
        }
        let Some(command) = self.registry.get(&id) else {
            tracing::error!(%id, "команды нет в реестре");
            return;
        };

        if self.dry_run {
            self.approval = Some(Approval {
                heard,
                note: PermissionNote::DryRun,
                answered_by: None,
            });
            self.apply(Event::Command(id));
            return;
        }

        match self.permissions.verdict(command) {
            Verdict::Allowed => {
                self.approval = Some(Approval {
                    heard,
                    note: PermissionNote::Saved,
                    answered_by: None,
                });
                self.apply(Event::Command(id));
            }
            Verdict::Denied => {
                tracing::info!(%id, "запрещено вами («никогда») — не выполняю");
                let approval = Approval {
                    heard,
                    note: PermissionNote::SavedNever,
                    answered_by: None,
                };
                self.write_journal(&approval, &id, "not_executed", None);
                self.play(Reply::Denied);
            }
            Verdict::Ask { offer_always } => {
                tracing::info!(%id, offer_always, "спрашиваю разрешение");
                self.pending = Some(Pending {
                    id: id.clone(),
                    heard,
                    offer_always,
                });
                self.apply(Event::NeedPermission(id));
            }
        }
    }

    fn on_dialog(&mut self, generation: u64, button: Option<i32>) {
        if generation != self.ask_generation
            || !matches!(self.state, State::AskingPermission { .. })
        {
            return; // окно уже закрыто ответом голосом или тайм-аутом
        }
        let answer = match button {
            Some(BUTTON_YES) => Answer::Once,
            Some(BUTTON_ALWAYS) => Answer::Always,
            Some(BUTTON_NEVER) => Answer::Never,
            // «Нет», крестик, Esc.
            _ => Answer::No,
        };
        tracing::info!(?answer, "ответ кнопкой");
        self.answer(answer, AnswerSource::Button);
    }

    fn answer(&mut self, answer: Answer, source: AnswerSource) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let note = match answer {
            Answer::Once => PermissionNote::Once,
            Answer::Always => PermissionNote::Always,
            Answer::No => PermissionNote::No,
            Answer::Never => PermissionNote::Never,
        };
        if !is_builtin(&pending.id)
            && let Some(command) = self.registry.get(&pending.id)
            && let Err(e) = self.permissions.record(command, answer)
        {
            tracing::error!(error = %jarvis_core::error_chain(&e), "не удалось сохранить разрешение");
        }
        let approval = Approval {
            heard: pending.heard,
            note: if is_builtin(&pending.id) && answer.allows() {
                PermissionNote::Builtin
            } else {
                note
            },
            answered_by: Some(source),
        };
        if answer.allows() {
            self.approval = Some(approval);
            self.apply(Event::PermissionGranted(pending.id));
        } else {
            self.write_journal(&approval, &pending.id, "not_executed", None);
            self.apply(Event::PermissionDenied);
        }
    }

    fn ask_user(&mut self, id: &str) {
        self.ask_generation += 1;
        let generation = self.ask_generation;
        let offer_always = self.pending.as_ref().is_some_and(|p| p.offer_always);
        let slots = self
            .pending
            .as_ref()
            .map(|p| p.heard.slots.clone())
            .unwrap_or_default();
        let question = if id == BUILTIN_FORGET_PERMISSIONS {
            speech::ask_forget_permissions_text()
        } else {
            match self.registry.get(id) {
                Some(command) => speech::ask_text(command, &slots),
                None => return,
            }
        };
        // Без голоса синтеза звучит «ask» из звукового пакета (см. `apply`).
        self.speak(question, Purpose::Ask(generation));
        let spec = if id == BUILTIN_FORGET_PERMISSIONS {
            DialogSpec {
                title: "Джарвис — подтверждение".into(),
                heading: "Забыть все разрешения?".into(),
                text: "Джарвис снова будет спрашивать перед каждой командой.\n\nОтветьте голосом («да» / «нет») или кнопкой."
                    .into(),
                buttons: vec![
                    DialogButton { id: BUTTON_YES, label: "Да, забыть".into() },
                    DialogButton { id: BUTTON_NO, label: "Нет".into() },
                ],
                default_button: BUTTON_NO,
            }
        } else {
            let Some(command) = self.registry.get(id) else {
                return;
            };
            permission_dialog(command, offer_always, &slots)
        };
        let events = self.events.clone();
        match jarvis_platform::show_dialog(spec, move |button| {
            let _ = events.send(CoreMsg::DialogAnswer { generation, button });
        }) {
            Ok(handle) => self.dialog = Some(handle),
            // Без окна остаётся ответ голосом — работаем дальше.
            Err(e) => tracing::error!(error = %e, "не удалось показать окно-вопрос"),
        }
    }

    fn close_dialog(&mut self) {
        self.ask_generation += 1;
        // Ответили кнопкой, пока вопрос ещё звучит, — договаривать незачем.
        if self
            .asking_aloud_until
            .take()
            .is_some_and(|until| Instant::now() < until)
        {
            self.player.stop();
        }
        if let Some(mut dialog) = self.dialog.take() {
            dialog.close();
        }
    }

    // ---------- выполнение ----------

    fn execute(&mut self, id: &str) {
        self.generation += 1;
        let generation = self.generation;
        let approval = self.approval.take().unwrap_or(Approval {
            heard: Heard {
                text: String::new(),
                score: 0.0,
                slots: SlotValues::new(),
                at: Instant::now(),
            },
            note: PermissionNote::Saved,
            answered_by: None,
        });

        if id == BUILTIN_FORGET_PERMISSIONS {
            self.executing = Some(Running {
                id: id.to_owned(),
                command: None,
                started: Instant::now(),
                deadline: ACTION_TIMEOUT,
                approval,
            });
            let result = match self.permissions.forget_all() {
                Ok(n) => {
                    tracing::info!(forgotten = n, "разрешения забыты (голосом)");
                    Ok(Outcome::Done)
                }
                Err(e) => Err(ActionError::Invalid(jarvis_core::error_chain(&e))),
            };
            let _ = self.events.send(CoreMsg::ActionDone { generation, result });
            return;
        }

        let Some(command) = self.registry.get(id).cloned() else {
            return;
        };
        // Слоты подставляются только сейчас — в манифесте и разрешении шаблон.
        let action = command.action.with_slots(&approval.heard.slots);
        tracing::info!(
            command = %command.id,
            action = %action.summary(),
            permission = ?approval.note,
            "выполняю"
        );
        let dry_run = approval.note == PermissionNote::DryRun;
        self.executing = Some(Running {
            id: command.id.clone(),
            command: Some(command.clone()),
            started: Instant::now(),
            deadline: expected_duration(&action),
            approval,
        });

        if dry_run {
            let _ = self.events.send(CoreMsg::ActionDone {
                generation,
                result: Ok(Outcome::Done),
            });
            return;
        }
        let cancel = CancelToken::default();
        self.cancel = Some(cancel.clone());
        let system = Arc::clone(&self.system);
        let events = self.events.clone();
        let dir = command.dir.clone();
        let spawned = std::thread::Builder::new()
            .name("jarvis-action".into())
            .spawn(move || {
                let play_events = events.clone();
                let play = move |sound: Sound| {
                    let _ = play_events.send(CoreMsg::PlaySound(sound));
                };
                let ctx = jarvis_actions::Context {
                    dir: &dir,
                    system: system.as_ref(),
                    cancel: &cancel,
                    play: &play,
                };
                // Паника в действии не должна ронять ядро (раздел 2.5 ТЗ).
                let result =
                    catch_unwind(AssertUnwindSafe(|| jarvis_actions::execute(&action, &ctx)))
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
        let reply_choice = running
            .command
            .as_ref()
            .map_or(ReplyChoice::Reply(Reply::Done), |c| c.reply);
        let reply = match result {
            Ok(outcome) => {
                tracing::info!(
                    command = %running.id,
                    action_ms = running.started.elapsed().as_millis() as u64,
                    since_heard_ms = running.approval.heard.at.elapsed().as_millis() as u64,
                    "готово"
                );
                self.write_journal(&running.approval, &running.id, "ok", None);
                match outcome {
                    Outcome::Play(sound) => {
                        self.play_sound(&sound);
                        None
                    }
                    Outcome::Done => match reply_choice {
                        ReplyChoice::Reply(r) => Some(r),
                        ReplyChoice::Silent => None,
                    },
                }
            }
            Err(e) => {
                tracing::error!(command = %running.id, error = %e, "не удалось");
                self.write_journal(
                    &running.approval,
                    &running.id,
                    "failed",
                    Some(e.to_string()),
                );
                Some(Reply::Failed)
            }
        };
        self.apply(Event::ActionFinished { reply });
    }

    // ---------- аварийная остановка и прочее ----------

    fn emergency_stop(&mut self, source: &str) {
        tracing::warn!(source, "аварийная остановка");
        if let Some(pending) = self.pending.take() {
            let approval = Approval {
                heard: pending.heard,
                note: PermissionNote::No,
                answered_by: None,
            };
            self.write_journal(
                &approval,
                &pending.id,
                "not_executed",
                Some("аварийная остановка".into()),
            );
        }
        if let Some(running) = self.executing.take() {
            self.write_journal(
                &running.approval,
                &running.id,
                "failed",
                Some("аварийная остановка".into()),
            );
        }
        self.apply(Event::EmergencyStop);
    }

    /// «Обнови команды»: перечитать пакеты без перезапуска. `false` — не
    /// загрузилось ни одной команды (старый набор остаётся).
    fn reload_commands(&mut self, source: &str) -> bool {
        let registry = jarvis_core::commands::Registry::load(&self.commands_dir);
        for problem in &registry.errors {
            tracing::error!("пропущено: {problem}");
        }
        for warning in &registry.warnings {
            tracing::warn!("{warning}");
        }
        if registry.commands.is_empty() {
            tracing::error!(
                source,
                "после перезагрузки ни одной команды — оставляю прежние"
            );
            return false;
        }
        tracing::info!(
            source,
            commands = registry.commands.len(),
            errors = registry.errors.len(),
            "команды перезагружены"
        );
        self.resolver =
            CommandResolver::new(&registry, self.wake_matcher.clone(), &self.config.nlu);
        if self.config.listen.hotwords_score > 0.0 {
            self.pipeline
                .set_hotwords(hotword_phrases(&registry, &self.config.wake.words));
        }
        self.registry = registry;
        true
    }

    fn forget_permissions(&mut self, source: &str) {
        match self.permissions.forget_all() {
            Ok(n) => {
                tracing::info!(forgotten = n, source, "разрешения забыты");
                self.play(Reply::Done);
            }
            Err(e) => {
                tracing::error!(error = %jarvis_core::error_chain(&e), "не удалось забыть разрешения");
                self.play(Reply::Failed);
            }
        }
    }

    fn write_journal(
        &self,
        approval: &Approval,
        command: &str,
        result: &'static str,
        error: Option<String>,
    ) {
        let Some(journal) = &self.journal else {
            return;
        };
        journal.write(&Entry {
            time: journal::now(),
            heard: approval.heard.text.clone(),
            command: command.to_owned(),
            slots: approval.heard.slots.clone(),
            score: approval.heard.score,
            permission: approval.note,
            answered_by: approval.answered_by,
            result,
            error,
        });
    }

    fn play(&mut self, reply: Reply) {
        if self.pack.variants(reply).is_empty() {
            return;
        }
        if let Some(duration) = self.player.play(reply) {
            self.muted_for(duration);
        }
    }

    /// Джарвис говорит `duration`: микрофон заглушён, и это время не должно
    /// съедать ожидание команды или ответа (учитывается в конце `apply`).
    fn muted_for(&mut self, duration: Duration) {
        self.gate.mute_for(duration + ECHO_TAIL);
        self.spoke_pending += duration + ECHO_TAIL;
    }

    /// Продлить ожидание на время своих фраз, накопленное с прошлого раза.
    fn flush_spoke(&mut self) {
        let spoke = std::mem::take(&mut self.spoke_pending);
        if !spoke.is_zero() {
            self.state = transition(
                self.state,
                Event::Spoke(spoke),
                Instant::now(),
                self.timings,
            )
            .next;
        }
    }

    /// Приветствие при запуске по времени суток («Доброе утро, сэр»), если
    /// оно есть в звуковом пакете.
    fn greet(&mut self) {
        let part = DayPart::from_hour(jarvis_core::journal::local_hour());
        let variants = self.pack.greetings(part);
        if variants.is_empty() {
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as usize);
        let index = nanos % variants.len();
        let clip = variants[index].clone();
        match self.player.play_clip(&clip) {
            Ok(d) => {
                tracing::info!(part = part.key(), "приветствие");
                self.muted_for(d);
            }
            Err(e) => {
                tracing::warn!(error = %jarvis_core::error_chain(&e), "не удалось поздороваться")
            }
        }
    }

    fn play_sound(&mut self, sound: &Sound) {
        let duration = match sound {
            Sound::Reply(key) => match Reply::ALL.into_iter().find(|r| r.key() == key) {
                Some(r) if !self.pack.variants(r).is_empty() => self.player.play(r),
                _ => None,
            },
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
            self.muted_for(d);
        }
    }

    // ---------- синтез речи ----------

    /// Отправляет фразу на синтез. `false` — голоса нет (не настроен, ещё
    /// грузится или не загрузился).
    fn speak(&mut self, text: String, purpose: Purpose) -> bool {
        // Подробности голосом выключены — вопрос и уточнение звучат фразами
        // звукового пакета (голосом пакета), подробности — в окне.
        let details = matches!(purpose, Purpose::Ask(_) | Purpose::Clarify);
        if details && !self.config.voice.spoken_details {
            return false;
        }
        let Some(speech) = self.speech.as_ref().filter(|_| self.speech_ready) else {
            return false;
        };
        self.speech_id += 1;
        self.speaking = Some(purpose);
        speech.say(self.speech_id, text);
        true
    }

    /// Ответ из звукового пакета, который заменяет синтезированная фраза.
    fn spoken_instead(&mut self, reply: Reply) -> bool {
        match reply {
            Reply::Ask => self.speech_ready && self.config.voice.spoken_details,
            Reply::NotUnderstood => std::mem::take(&mut self.clarifying),
            _ => false,
        }
    }

    fn on_speech(&mut self, event: SpeechEvent) {
        match event {
            SpeechEvent::Ready { voice, load_time } => {
                tracing::info!(%voice, ms = load_time.as_millis() as u64, "голос синтеза загружен");
                self.speech_ready = true;
                if std::mem::take(&mut self.warn_elevated) {
                    self.speak(speech::elevated_warning_text(), Purpose::Notice);
                }
            }
            SpeechEvent::Unavailable(why) => {
                tracing::error!(%why, "голос синтеза не загрузился — отвечаю звуковым пакетом");
                self.speech = None;
                self.speech_ready = false;
            }
            SpeechEvent::Clip {
                id,
                clip,
                synth_time,
            } => {
                let Some(purpose) = self.take_speaking(id) else {
                    return;
                };
                match self.player.play_clip(&clip) {
                    Ok(duration) => {
                        tracing::debug!(
                            ?purpose,
                            synth_ms = synth_time.as_millis() as u64,
                            secs = duration.as_secs_f32(),
                            "говорю"
                        );
                        self.gate.mute_for(duration + ECHO_TAIL);
                        if matches!(purpose, Purpose::Ask(_)) {
                            self.asking_aloud_until = Some(Instant::now() + duration);
                        }
                        self.apply(Event::Spoke(duration + ECHO_TAIL));
                    }
                    Err(e) => {
                        tracing::error!(error = %jarvis_core::error_chain(&e), "не удалось проиграть фразу");
                        self.speech_fallback(purpose);
                    }
                }
            }
            SpeechEvent::Failed { id, why } => {
                if let Some(purpose) = self.take_speaking(id) {
                    tracing::error!(%why, ?purpose, "синтез не удался");
                    self.speech_fallback(purpose);
                }
            }
        }
    }

    /// Назначение готовой фразы, если она всё ещё к месту.
    fn take_speaking(&mut self, id: u64) -> Option<Purpose> {
        if id != self.speech_id {
            return None;
        }
        let purpose = self.speaking.take()?;
        let relevant = match purpose {
            Purpose::Ask(generation) => {
                generation == self.ask_generation
                    && matches!(self.state, State::AskingPermission { .. })
            }
            Purpose::Clarify => matches!(self.state, State::Listening { .. }),
            Purpose::Notice => self.state != State::Paused,
        };
        relevant.then_some(purpose)
    }

    fn speech_fallback(&mut self, purpose: Purpose) {
        match purpose {
            Purpose::Ask(_) => self.play(Reply::Ask),
            Purpose::Clarify => self.play(Reply::NotUnderstood),
            Purpose::Notice => {}
        }
    }

    fn apply(&mut self, event: Event) {
        // Звуки вне `apply` (шаг цепочки, «команды обновлены») — учесть до таймеров.
        self.flush_spoke();
        let before = self.state;
        let tr = transition(self.state, event.clone(), Instant::now(), self.timings);
        self.state = tr.next;

        // Тишина в ответ на вопрос — не выполнять, записать в журнал.
        if matches!(before, State::AskingPermission { .. })
            && self.state == State::Idle
            && event == Event::Tick
            && let Some(pending) = self.pending.take()
        {
            tracing::info!(command = %pending.id, "ответа нет — не выполняю");
            let approval = Approval {
                heard: pending.heard,
                note: PermissionNote::Timeout,
                answered_by: None,
            };
            self.write_journal(&approval, &pending.id, "not_executed", None);
        }

        for effect in tr.effects {
            match effect {
                Effect::PlayReply(reply) if self.spoken_instead(reply) => {}
                Effect::PlayReply(reply) => self.play(reply),
                Effect::SetListening(on) => self.pipeline.set_listening(on),
                Effect::SetTranscribing(on) => {
                    self.early.clear();
                    self.pipeline.set_transcribing(on);
                }
                Effect::Execute(id) => self.execute(&id),
                Effect::AskUser(id) => self.ask_user(&id),
                Effect::CloseAsk => self.close_dialog(),
                Effect::StopAll => {
                    self.player.stop();
                    self.speaking = None;
                    self.asking_aloud_until = None;
                    // Скрипт — завершить (всё дерево процессов), ожидания — прервать.
                    if let Some(cancel) = self.cancel.take() {
                        cancel.cancel();
                    }
                    // Зажатые клавиши — отпустить сразу, не дожидаясь потока действия.
                    jarvis_platform::release_all_keys();
                    self.generation += 1; // результат прерванного действия — в никуда
                    self.executing = None;
                    self.approval = None;
                    self.early.clear();
                }
            }
        }
        // Ответ, прозвучавший в этом переходе («Да, сэр»), не съедает ожидание.
        self.flush_spoke();
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
            (State::AskingPermission { .. }, None, true) => Indicator::Asking,
            (State::Listening { .. } | State::Executing | State::FollowUp { .. }, None, true) => {
                Indicator::Listening
            }
        };
        if self.ui.send(indicator).is_ok() {
            self.waker.wake();
        }
    }
}

/// Текст окна-вопроса: что именно произойдёт (раздел 2.1 ТЗ) — с уже
/// подставленными значениями слотов.
fn permission_dialog(command: &Command, offer_always: bool, slots: &SlotValues) -> DialogSpec {
    let phrase = speech::filled_phrase(command, slots);
    let description = jarvis_core::slots::fill(&command.description, slots);
    let action = command.action.with_slots(slots);
    let details = match &action {
        ActionSpec::OpenUrl { url } => format!("Адрес: {url}"),
        ActionSpec::LaunchApp { path, args, .. } => {
            format!(
                "Программа: {path}\nАргументы: {}",
                if args.is_empty() {
                    "нет".into()
                } else {
                    args.join(" ")
                }
            )
        }
        ActionSpec::RunScript {
            path,
            args,
            timeout_sec,
        } => format!(
            "Скрипт: {}\nАргументы: {}\nНе дольше {} с",
            command.dir.join(path).display(),
            if args.is_empty() {
                "нет".into()
            } else {
                args.join(" ")
            },
            timeout_sec.unwrap_or(jarvis_core::commands::DEFAULT_SCRIPT_TIMEOUT_SEC)
        ),
        other => format!("Действие: {}", other.summary()),
    };
    let mut buttons = vec![DialogButton {
        id: BUTTON_YES,
        label: "Да".into(),
    }];
    if offer_always {
        buttons.push(DialogButton {
            id: BUTTON_ALWAYS,
            label: "Всегда".into(),
        });
    }
    buttons.push(DialogButton {
        id: BUTTON_NO,
        label: "Нет".into(),
    });
    buttons.push(DialogButton {
        id: BUTTON_NEVER,
        label: "Никогда".into(),
    });
    let voice = if offer_always {
        "«да», «всегда», «нет» или «никогда»"
    } else {
        "«да», «нет» или «никогда» (эта команда всегда спрашивает)"
    };
    DialogSpec {
        title: "Джарвис — разрешение".into(),
        heading: format!(
            "Сэр, команда «{phrase}» {}.",
            description.trim().trim_end_matches('.')
        ),
        text: format!(
            "{details}\nПакет команд: {} ({})\n\nРазрешить? Ответьте голосом — {voice} — или кнопкой. \
             Без ответа через 10 секунд команда не выполнится.",
            command.pack, command.id
        ),
        buttons,
        default_button: BUTTON_NO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::commands::MediaOp;
    use std::path::PathBuf;

    fn command(always_ask: bool, action: ActionSpec) -> Command {
        Command {
            id: "open_youtube".into(),
            pack: "browser".into(),
            phrases: vec!["открой ютуб".into()],
            description: "откроет YouTube в браузере".into(),
            reply: ReplyChoice::Reply(Reply::Done),
            always_ask,
            slots: Default::default(),
            action,
            dir: PathBuf::from("commands/browser"),
        }
    }

    #[test]
    fn dialog_shows_filled_slots_and_script_paths() {
        let mut c = command(
            false,
            ActionSpec::RunScript {
                path: "backup.ps1".into(),
                args: vec!["--level={level}".into()],
                timeout_sec: Some(60),
            },
        );
        c.dir = PathBuf::from("C:\\jarvis\\commands\\my");
        let slots: SlotValues = [("level".to_owned(), "40".to_owned())]
            .into_iter()
            .collect();
        let spec = permission_dialog(&c, true, &slots);
        assert!(spec.text.contains("backup.ps1"), "{}", spec.text);
        assert!(spec.text.contains("--level=40"), "{}", spec.text);
        assert!(spec.text.contains("60 с"), "{}", spec.text);
    }

    #[test]
    fn dialog_names_what_will_happen() {
        let spec = permission_dialog(
            &command(
                false,
                ActionSpec::OpenUrl {
                    url: "https://www.youtube.com".into(),
                },
            ),
            true,
            &SlotValues::new(),
        );
        assert!(
            spec.heading.contains("открой ютуб") && spec.heading.contains("YouTube"),
            "{}",
            spec.heading
        );
        assert!(
            spec.text.contains("https://www.youtube.com"),
            "{}",
            spec.text
        );
        let labels: Vec<_> = spec.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Да", "Всегда", "Нет", "Никогда"]);
        assert_eq!(spec.default_button, BUTTON_NO, "Enter не должен разрешать");
    }

    #[test]
    fn always_ask_command_has_no_always_button() {
        let spec = permission_dialog(
            &command(
                true,
                ActionSpec::Media {
                    operation: MediaOp::Mute,
                    steps: None,
                    level: None,
                },
            ),
            false,
            &SlotValues::new(),
        );
        assert!(!spec.buttons.iter().any(|b| b.id == BUTTON_ALWAYS));
        assert!(spec.text.contains("всегда спрашивает"));
    }
}
