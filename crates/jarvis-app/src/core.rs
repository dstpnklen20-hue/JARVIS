//! Ядро: автомат состояний, команды, разрешения, журнал, звук ответа.
//! Работает в потоке `jarvis-core`; события приходят через канал.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_actions::{ActionError, Outcome, Sound, System};
use jarvis_app::resolver::{
    BUILTIN_FORGET_PERMISSIONS, BUILTIN_STOP, CommandResolver, EarlyDecider, Resolution, is_builtin,
};
use jarvis_app::system::PlatformSystem;
use jarvis_audio::pack::VoicePack;
use jarvis_audio::pipeline::{MuteGate, Pipeline, PipelineEvent, PipelineSettings, WakeEvent};
use jarvis_audio::player::Player;
use jarvis_audio::stt::Transcript;
use jarvis_core::Config;
use jarvis_core::commands::{ActionSpec, Command, Registry, ReplyChoice};
use jarvis_core::journal::{self, ActionJournal, AnswerSource, Entry, PermissionNote};
use jarvis_core::permissions::{Answer, PermissionStore, Verdict};
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
    TogglePause,
    EmergencyStop,
    ForgetPermissions,
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
    pub ui: Sender<Indicator>,
    pub waker: Waker,
}

/// Что услышали — для журнала.
#[derive(Debug, Clone)]
struct Heard {
    text: String,
    score: f32,
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
    approval: Approval,
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
        })
    }

    pub fn run(&mut self, rx: Receiver<CoreMsg>) {
        loop {
            match rx.recv_timeout(CORE_TICK) {
                Ok(CoreMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(CoreMsg::TogglePause) => self.apply(Event::TogglePause),
                Ok(CoreMsg::EmergencyStop) => self.emergency_stop("горячая клавиша"),
                Ok(CoreMsg::ForgetPermissions) => self.forget_permissions("трей"),
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
        let now = Instant::now();
        if self.state.accepts_commands()
            && let Some((utterance, id)) = self.early.poll(now)
        {
            tracing::debug!(utterance, %id, "раннее решение по частичному тексту");
            let score = self.early_score();
            self.command(id, String::new(), score, now);
        }
        if let Some(running) = &self.executing
            && running.started.elapsed() > ACTION_TIMEOUT
        {
            tracing::error!(command = %running.id, "действие не ответило за {ACTION_TIMEOUT:?}");
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
                self.command(id, t.text, score, now);
            }
            return;
        }
        if !self.early.on_final(t.utterance) {
            return; // по этой фразе команда уже выполнена раньше
        }
        tracing::info!(text = %t.text, ?resolution, "фраза");
        match resolution {
            Resolution::Command { id, score } => self.command(id, t.text, score, now),
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

    // ---------- команды и разрешения ----------

    fn command(&mut self, id: String, text: String, score: f32, at: Instant) {
        self.early.clear();
        let heard = Heard { text, score, at };

        if id == BUILTIN_STOP {
            self.emergency_stop("голос");
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
            permission_dialog(command, offer_always)
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
        tracing::info!(
            command = %command.id,
            action = %command.action.summary(),
            permission = ?approval.note,
            "выполняю"
        );
        let dry_run = approval.note == PermissionNote::DryRun;
        self.executing = Some(Running {
            id: command.id.clone(),
            command: Some(command.clone()),
            started: Instant::now(),
            approval,
        });

        if dry_run {
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
            self.gate.mute_for(duration + ECHO_TAIL);
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
            self.gate.mute_for(d + ECHO_TAIL);
        }
    }

    fn apply(&mut self, event: Event) {
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
                    self.generation += 1; // результат прерванного действия — в никуда
                    self.executing = None;
                    self.approval = None;
                    self.early.clear();
                    // Отпускание клавиш и завершение скриптов — этап 4.
                }
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

/// Текст окна-вопроса: что именно произойдёт (раздел 2.1 ТЗ).
fn permission_dialog(command: &Command, offer_always: bool) -> DialogSpec {
    let phrase = command
        .phrases
        .first()
        .map_or(command.id.as_str(), String::as_str);
    let details = match &command.action {
        ActionSpec::OpenUrl { url } => format!("Адрес: {url}"),
        ActionSpec::Media { .. } | ActionSpec::Say { .. } => command.action.summary(),
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
        heading: format!("Сэр, команда «{phrase}» {}.", command.description),
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
            action,
            dir: PathBuf::from("commands/browser"),
        }
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
                },
            ),
            false,
        );
        assert!(!spec.buttons.iter().any(|b| b.id == BUTTON_ALWAYS));
        assert!(spec.text.contains("всегда спрашивает"));
    }
}
