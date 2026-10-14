//! Ядро: автомат состояний, команды, разрешения, журнал, звук ответа.
//! Работает в потоке `jarvis-core`; события приходят через канал.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use std::path::PathBuf;

use anyhow::Context as _;
use jarvis_actions::{ActionError, CancelToken, NoteTarget, Outcome, Question, Sound, System};
use jarvis_app::answer::{SpeechQueue, thinking_cue_due};
use jarvis_app::brain::{Brain, BrainEvent, Failure, ModelFactory, slot_values, tool_commands};
use jarvis_app::resolver::{
    BUILTIN_CLOSE_ANSWER, BUILTIN_DIALOG_OFF, BUILTIN_DIALOG_ON, BUILTIN_FORGET_PERMISSIONS,
    BUILTIN_RELOAD_COMMANDS, BUILTIN_SLEEP, BUILTIN_STOP, CommandResolver, EXACT_SCORE,
    EarlyDecider, Resolution, hotword_phrases, is_builtin, is_dialog_pair, spoken_phrase,
};
use jarvis_app::speech::{self, Speech, SpeechEvent};
use jarvis_app::system::PlatformSystem;
use jarvis_audio::pack::{DayPart, VoicePack};
use jarvis_audio::pipeline::{MuteGate, Pipeline, PipelineEvent, PipelineSettings, WakeEvent};
use jarvis_audio::player::Player;
use jarvis_audio::stt::Transcript;
use jarvis_audio::tts::{SynthParams, VoiceModel};
use jarvis_audio::wake::WakeMatcher;
use jarvis_audio::wav::Clip;
use jarvis_core::Config;
use jarvis_core::commands::{ActionSpec, Command, Registry, ReplyChoice};
use jarvis_core::journal::{self, ActionJournal, AnswerSource, Entry, PermissionNote};
use jarvis_core::permissions::{Answer, PermissionStore, Verdict};
use jarvis_core::slots::SlotValues;
use jarvis_core::state::{Effect, Event, Reply, State, Timings, transition};
use jarvis_llm::claude::{ClaudeClient, ClaudeSettings, messages_url};
use jarvis_llm::conversation::ToolOutcome;
use jarvis_llm::tools::CommandRequest;
use jarvis_llm::{LanguageModel, LlmError};
use jarvis_nlu::answers::{PermissionReply, parse_reply};
use jarvis_nlu::{CommandMatcher, CommandPhrases, MatchResult, Thresholds};
use jarvis_platform::{
    AnswerStatus, AnswerView, AnswerWindow, DialogButton, DialogHandle, DialogSpec, Waker,
};

use crate::tray::{Indicator, UiMsg};

/// Как часто ядро проверяет таймауты и раннее решение.
const CORE_TICK: Duration = Duration::from_millis(50);
/// После своей фразы ещё немного не слушаем: эхо комнаты.
const ECHO_TAIL: Duration = Duration::from_millis(150);
/// Сколько ждём действие, прежде чем сказать «не удалось».
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём ответа на вопрос-разрешение (раздел 2.1 ТЗ).
const ASK_TIMEOUT: Duration = Duration::from_secs(10);
/// Номера предложений ответа нейросети — отдельно от номеров обычных фраз.
const SENTENCE_ID_BASE: u64 = 1 << 48;
/// Нейросеть молчит дольше — считать ответ сорвавшимся.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

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
    /// Поток нейросети.
    Brain(BrainEvent),
    /// Заметка для диктовки открыта и видна (или нет — почему).
    NoteReady(Result<(), String>),
    /// Надиктованное записано в заметку (или нет — почему).
    NoteDone(Result<(), String>),
    TogglePause,
    EmergencyStop,
    ForgetPermissions,
    ReloadCommands,
    /// Главное окно просит список команд с разрешениями.
    WindowCommands,
    /// Редактор команд изменил личный пакет — перечитать команды.
    CommandsChanged,
    /// Главное окно: «ask» (сбросить решение), «always» или «never».
    SetPermission {
        id: String,
        value: String,
    },
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
    pub ui: Sender<UiMsg>,
    pub waker: Waker,
}

pub struct VoiceSetup {
    pub name: String,
    pub model: VoiceModel,
    pub params: SynthParams,
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

/// Команда спросила (`ask`) и ждёт ответа: варианты → команды.
struct Choice {
    matcher: CommandMatcher,
    /// id команды по индексу в `matcher`.
    ids: Vec<String>,
}

impl Choice {
    fn new(question: &Question, thresholds: Thresholds) -> Self {
        let mut ids: Vec<String> = Vec::new();
        let mut entries: Vec<CommandPhrases> = Vec::new();
        for (answer, id) in &question.choices {
            match ids.iter().position(|known| known == id) {
                Some(i) => entries[i].phrases.push(answer.clone()),
                None => {
                    ids.push(id.clone());
                    entries.push(CommandPhrases {
                        phrases: vec![answer.clone()],
                        slots: Vec::new(),
                    });
                }
            }
        }
        Self {
            matcher: CommandMatcher::with_slots(&entries, thresholds),
            ids,
        }
    }

    /// Команда по ответу и уверенность.
    fn resolve(&self, text: &str) -> Option<(String, f32)> {
        match self.matcher.find(text) {
            MatchResult::Matched(c) => Some((self.ids.get(c.command)?.clone(), c.score)),
            _ => None,
        }
    }
}

/// Текущий ответ нейросети.
struct AnswerTurn {
    generation: u64,
    question: String,
    asked: Instant,
    queue: SpeechQueue<Arc<Clip>>,
    next_sentence: u64,
    /// Уже зазвучало хоть что-то (ответ или «Секунду, сэр»).
    spoken: bool,
    cue_played: bool,
    /// Нейросеть закончила (ответом или ошибкой).
    done: bool,
    failure: Option<Failure>,
    commands: Vec<CommandRequest>,
}

/// Откуда аварийная остановка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Горячая клавиша: всё остановить и выключить микрофон (пауза).
    Hotkey,
    /// «Джарвис, стоп»: всё остановить, но слушать дальше — иначе голосом
    /// из паузы не выйти, и Джарвис «глохнет» до перезапуска.
    Voice,
}

impl Stop {
    fn source(self) -> &'static str {
        match self {
            Stop::Hotkey => "горячая клавиша",
            Stop::Voice => "голос",
        }
    }
}

/// Что услышали — для журнала и подстановки слотов.
#[derive(Debug, Clone)]
struct Heard {
    text: String,
    score: f32,
    slots: SlotValues,
    at: Instant,
}

/// Диктовка в заметку: части текста по фразам; пауза после последней —
/// записать.
///
/// Порядок (ваш, 2026-10-15): заметка открывается и проверяется (не меньше
/// [`NOTE_MIN_WAIT`]) → ещё [`NOTE_SETTLE`] → «Слушаю, сэр» → диктовка →
/// [`DICTATION_PAUSE`] тишины → запись → «Записал, сэр».
struct Dictation {
    target: NoteTarget,
    started: Instant,
    /// Заметка готова — когда сказать «Слушаю, сэр».
    listen_at: Option<Instant>,
    /// «Слушаю, сэр» сказано, ждём текст.
    listening: Option<Instant>,
    /// Фразы с этими номерами и раньше — не диктовка (сама команда «запиши
    /// идею» дописывается распознавателем уже после её выполнения).
    ignore_upto: Option<u64>,
    parts: Vec<String>,
    /// Когда человек последний раз говорил.
    heard_at: Option<Instant>,
    commit_at: Option<Instant>,
}

/// Не меньше стольких секунд ждать открытия заметки (браузер, страница Keep).
const NOTE_MIN_WAIT: Duration = Duration::from_secs(5);
/// После того как заметка точно готова — ещё столько до «Слушаю, сэр».
const NOTE_SETTLE: Duration = Duration::from_secs(2);
/// Сколько тишины после последней фразы считать концом диктовки.
const DICTATION_PAUSE: Duration = Duration::from_secs(3);
/// Сколько ждать начала диктовки после «Слушаю, сэр».
const DICTATION_START_WAIT: Duration = Duration::from_secs(10);

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
        ActionSpec::WaitProcess { timeout_sec, .. } => {
            Duration::from_secs(*timeout_sec) + ACTION_TIMEOUT
        }
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
    ui: Sender<UiMsg>,
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
    /// Ждём ответ на вопрос команды (`ask`).
    choice: Option<Choice>,
    warn_elevated: bool,
    /// Нейросеть (`llm.enabled`).
    brain: Option<Brain>,
    answer: Option<AnswerTurn>,
    /// Номер вопроса: события старого ответа — в никуда.
    llm_generation: u64,
    /// Сразу после ответа нейросети фраза без «Джарвис» — тоже вопрос (до IDLE).
    llm_follow_up: bool,
    /// «Джарвис» прозвучал, первой фразы после него ещё не было.
    after_wake: bool,
    /// Идёт диктовка в заметку («запиши идею» → «Слушаю, сэр»).
    dictation: Option<Dictation>,
    /// Номер последней распознанной фразы.
    last_utterance: Option<u64>,
    /// Номер первой фразы после «Джарвис»: она начинается с самого слова
    /// активации (см. `CommandResolver::strip_wake_lead`).
    wake_utterance: Option<u64>,
    /// Команды, которые попросила нейросеть, — по одной.
    llm_commands: VecDeque<CommandRequest>,
    /// Номер вызова команды нейросети, которая сейчас выполняется: её итог
    /// (из журнала) уходит нейросети.
    llm_tool: RefCell<Option<String>>,
    /// Окно с вопросом и ответом (`llm.show_window`) и когда его закрыть.
    text_window: Option<AnswerWindow>,
    text_window_close_at: Option<Instant>,
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
                voice.params,
                Box::new(move |event| {
                    let _ = speech_events.send(CoreMsg::Speech(event));
                }),
            )
            .inspect_err(|e| tracing::error!(error = %e, "не удалось запустить поток синтеза"))
            .ok()
        });
        let brain = p
            .config
            .llm
            .enabled
            .then(|| spawn_brain(&p.config, &p.registry, events.clone()))
            .flatten();
        let nlu = &p.config.nlu;
        Ok(Self {
            state: State::Idle,
            timings: Timings {
                listen: Duration::from_secs_f32(p.config.wake.listen_secs),
                follow_up: Duration::from_secs(u64::from(p.config.session.follow_up_secs)),
                ask: ASK_TIMEOUT,
                dialog: None,
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
            choice: None,
            warn_elevated: p.elevated,
            brain,
            answer: None,
            llm_generation: 0,
            llm_follow_up: false,
            after_wake: false,
            dictation: None,
            last_utterance: None,
            wake_utterance: None,
            llm_commands: VecDeque::new(),
            llm_tool: RefCell::new(None),
            text_window: None,
            text_window_close_at: None,
        })
    }

    pub fn run(&mut self, rx: Receiver<CoreMsg>) {
        loop {
            match rx.recv_timeout(CORE_TICK) {
                Ok(CoreMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(CoreMsg::TogglePause) => self.apply(Event::TogglePause),
                Ok(CoreMsg::EmergencyStop) => self.emergency_stop(Stop::Hotkey),
                Ok(CoreMsg::ForgetPermissions) => self.forget_permissions("трей"),
                Ok(CoreMsg::WindowCommands) => self.send_window_commands(),
                Ok(CoreMsg::CommandsChanged) => {
                    self.reload_commands("редактор команд");
                    self.send_window_commands();
                }
                Ok(CoreMsg::SetPermission { id, value }) => {
                    self.set_permission(&id, &value);
                    self.send_window_commands();
                }
                Ok(CoreMsg::ReloadCommands) => {
                    let ok = self.reload_commands("трей");
                    self.play(if ok { Reply::Done } else { Reply::Failed });
                }
                Ok(CoreMsg::PlaySound(sound)) => self.play_sound(&sound),
                Ok(CoreMsg::Speech(event)) => self.on_speech(event),
                Ok(CoreMsg::Brain(event)) => self.on_brain(event),
                Ok(CoreMsg::NoteReady(result)) => self.on_note_ready(result),
                Ok(CoreMsg::NoteDone(result)) => self.on_note_done(result),
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
        self.tick_answer(now);
        self.next_llm_command(now);
        self.tick_dictation(now);
        if self.text_window_close_at.is_some_and(|at| now >= at) {
            self.text_window_close_at = None;
            if let Some(window) = self.text_window.take() {
                window.close();
            }
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
                self.send_ui(UiMsg::Mic(device));
                self.refresh_ui();
            }
            PipelineEvent::Wake(wake) => self.on_wake(wake),
            PipelineEvent::Transcript(t) => self.on_transcript(t),
            // Человек начал говорить — ожидание команды не обрывается, пока
            // слова ещё не распознаны.
            PipelineEvent::Voice => {
                if self.state.accepts_commands() {
                    self.apply(Event::UserSpeaking);
                }
            }
            PipelineEvent::Rejected {
                heard,
                similarity,
                threshold,
                level_db,
            } => {
                tracing::info!(
                    %heard,
                    similarity,
                    threshold,
                    level_db = format_args!("{level_db:.0}"),
                    "«Джарвис» сказал не ваш голос — игнорирую"
                );
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
            State::Idle
                | State::Listening { .. }
                | State::FollowUp { .. }
                | State::Conversation { .. }
        );
        if !answering {
            // Промолчит — пусть в логе будет видно почему.
            tracing::info!(heard = %wake.heard, state = ?self.state, "«Джарвис» услышан, но сейчас не отвечаю");
        }
        self.apply(Event::WakeWord);
        if answering {
            self.after_wake = true;
            self.wake_utterance = None;
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
                level_db = format_args!("{:.0}", wake.level_db),
                "«Джарвис» — отвечаю"
            );
        }
    }

    fn on_transcript(&mut self, mut t: Transcript) {
        self.last_utterance = Some(t.utterance);
        if matches!(self.state, State::Idle | State::Paused) {
            self.early.clear();
            return;
        }
        // Первая фраза после «Джарвис» начинается с него самого — как бы его
        // ни записал распознаватель («гарвес», «с»), это не команда.
        if std::mem::take(&mut self.after_wake) {
            self.wake_utterance = Some(t.utterance);
        }
        if self.wake_utterance == Some(t.utterance) {
            t.text = self.resolver.strip_wake_lead(&t.text);
        }
        let now = Instant::now();
        let resolution = self.resolver.resolve(&t.text);

        // «Стоп» — в любом активном состоянии, даже во время вопроса и действия.
        // По частичному тексту — только точное слово: «сто…» ещё может
        // оказаться «столом», а не «стопом».
        // Пока Джарвис отвечает, распознаватель может слышать и его голос из
        // колонок — «стоп» тогда в конце чужой фразы: проверяем и хвост.
        let stop_in_tail = matches!(self.state, State::Executing) && {
            let words = jarvis_audio::wake::normalize_words(&t.text);
            (1..=2.min(words.len())).any(|n| {
                let tail = words[words.len() - n..].join(" ");
                matches!(self.resolver.resolve(&tail), Resolution::Command { id, score, .. }
                    if id == BUILTIN_STOP && score >= EXACT_SCORE)
            })
        };
        if stop_in_tail
            || matches!(&resolution, Resolution::Command { id, score, .. }
            if id == BUILTIN_STOP && (t.is_final || *score >= EXACT_SCORE))
        {
            self.emergency_stop(Stop::Voice);
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

        // Диктовка: всё сказанное — текст заметки, не команды.
        if self.dictation.is_some() && matches!(self.state, State::Listening { .. }) {
            self.on_dictation(&t, now);
            return;
        }

        if !self.state.accepts_commands() {
            return;
        }
        if !t.is_final {
            tracing::trace!(text = %t.text, ?resolution, "частичный текст");
            if !t.text.trim().is_empty() {
                self.send_ui(UiMsg::Heard(t.text.clone()));
                self.apply(Event::UserSpeaking);
            }
            // Ответ на вопрос — только по итогу фразы: короткое «кс» по началу
            // фразы не отличить, а чужая команда не должна сработать раньше ответа.
            if self.choice.is_some() {
                return;
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
        if !t.text.trim().is_empty() {
            self.send_ui(UiMsg::Heard(t.text.clone()));
        }
        if !self.early.on_final(t.utterance) {
            return; // по этой фразе команда уже выполнена раньше
        }
        if let Some(choice) = &self.choice
            && let Some((id, score)) = choice.resolve(&self.resolver.strip_wake(&t.text))
        {
            tracing::info!(text = %t.text, %id, score, "ответ на вопрос");
            self.choice = None;
            self.command(id, t.text, score, SlotValues::new(), now);
            return;
        }
        tracing::info!(text = %t.text, ?resolution, "фраза");
        match resolution {
            Resolution::Command { id, score, slots } => self.command(id, t.text, score, slots, now),
            Resolution::Ambiguous { first, second } if is_dialog_pair(&first, &second) => {
                // «Включи / выключи режим диалога» — решает состояние, не вопрос.
                tracing::info!("режим диалога: включи или выключи — решаю по состоянию");
                self.command(
                    BUILTIN_DIALOG_ON.to_owned(),
                    t.text,
                    1.0,
                    SlotValues::new(),
                    now,
                );
            }
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
                if self.wants_llm(&t.text) {
                    self.ask_llm(&t.text, now);
                    return;
                }
                tracing::info!(?best, "не понял");
                self.apply(Event::NotUnderstood);
            }
            Resolution::OutOfRange { id } => {
                tracing::info!(%id, text = %t.text, "значение вне допустимого диапазона");
                self.apply(Event::InvalidValue);
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
            self.emergency_stop(Stop::Voice);
            return;
        }
        if id == BUILTIN_SLEEP {
            // Безвредная встроенная — без вопроса. Включить снова — щелчок по
            // реактору или «Пауза» в трее (голосом нельзя: микрофон выключен).
            let approval = Approval {
                heard,
                note: PermissionNote::Builtin,
                answered_by: None,
            };
            self.write_journal(&approval, &id, "ok", None);
            if !self.speak("Отключаюсь, сэр.".into(), Purpose::Notice) {
                self.play(Reply::Done);
            }
            self.apply(Event::Pause);
            return;
        }
        if id == BUILTIN_CLOSE_ANSWER {
            let approval = Approval {
                heard,
                note: PermissionNote::Builtin,
                answered_by: None,
            };
            let closed = self.text_window.take().is_some_and(|w| {
                w.close();
                true
            });
            self.text_window_close_at = None;
            self.write_journal(&approval, &id, if closed { "ok" } else { "nothing" }, None);
            // Молча, как выполненная команда: дальше — ожидание следующей.
            self.apply(Event::Question);
            self.apply(Event::ActionFinished { reply: None });
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
        if id == BUILTIN_DIALOG_ON || id == BUILTIN_DIALOG_OFF {
            // Безвредные встроенные — без вопроса. В режиме диалога любая из
            // фраз — выход: включать включённое незачем, а «включи» и
            // «выключи» распознаватель путает (две буквы разницы).
            let on = id == BUILTIN_DIALOG_ON && self.timings.dialog.is_none();
            tracing::info!(on, "режим диалога");
            self.timings.dialog = on
                .then(|| Duration::from_secs(u64::from(self.config.session.dialog_idle_mins) * 60));
            let approval = Approval {
                heard,
                note: PermissionNote::Builtin,
                answered_by: None,
            };
            self.write_journal(&approval, &id, "ok", None);
            self.apply(if on {
                Event::DialogStart
            } else {
                Event::DialogEnd
            });
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
                    Outcome::Ask(question) => {
                        self.ask_question(&question);
                        return;
                    }
                    Outcome::Dictate(target) => {
                        self.start_dictation(target);
                        return;
                    }
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

    /// «Запиши идею»: открыть заметку и дождаться, пока она точно готова;
    /// «Слушаю, сэр» — позже, в `tick_dictation`. Пока ждём — состояние
    /// «выполняется»: команды не принимаются, «стоп» — да.
    fn start_dictation(&mut self, target: NoteTarget) {
        tracing::info!(note = %target.title, "диктовка в заметку — открываю заметку");
        let (url, title) = (target.url.clone(), target.title.clone());
        let events = self.events.clone();
        let opened = std::thread::Builder::new()
            .name("jarvis-note-open".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(|| jarvis_platform::keep_open_note(&url, &title))
                        .unwrap_or_else(|_| Err("открытие заметки завершилось паникой".into()));
                let _ = events.send(CoreMsg::NoteReady(result));
            });
        if let Err(e) = opened {
            let _ = self.events.send(CoreMsg::NoteReady(Err(format!(
                "не удалось запустить поток: {e}"
            ))));
        }
        self.dictation = Some(Dictation {
            target,
            started: Instant::now(),
            listen_at: None,
            listening: None,
            ignore_upto: None,
            parts: Vec::new(),
            heard_at: None,
            commit_at: None,
        });
    }

    fn on_note_ready(&mut self, result: Result<(), String>) {
        let Some(d) = self.dictation.as_mut() else {
            return; // диктовку уже отменили («стоп»)
        };
        match result {
            Ok(()) => {
                tracing::info!(
                    open_ms = d.started.elapsed().as_millis() as u64,
                    "заметка готова"
                );
                let ready = Instant::now().max(d.started + NOTE_MIN_WAIT);
                d.listen_at = Some(ready + NOTE_SETTLE);
            }
            Err(e) => {
                tracing::warn!(error = %e, "заметка не открылась — диктовки не будет");
                self.dictation = None;
                self.apply(Event::ActionFinished {
                    reply: Some(Reply::Failed),
                });
            }
        }
    }

    fn tick_dictation(&mut self, now: Instant) {
        let Some(d) = self.dictation.as_ref() else {
            return;
        };
        match d.listening {
            None if d.listen_at.is_some_and(|at| now >= at) => self.begin_dictation(),
            None => {}
            Some(_) if d.commit_at.is_some_and(|at| now >= at) => self.commit_dictation(),
            // Пока человек диктует (и первые секунды после «Слушаю, сэр») —
            // не отпускать ожидание: паузы в мысли длиннее обычного.
            Some(since)
                if d.heard_at.is_some() || now.duration_since(since) < DICTATION_START_WAIT =>
            {
                if matches!(self.state, State::Listening { .. }) {
                    self.apply(Event::UserSpeaking);
                }
            }
            Some(_) => {} // молчание — ожидание кончится само («Не расслышал, сэр»)
        }
    }

    /// Заметка готова: распознавание — с чистого листа, «Слушаю, сэр».
    fn begin_dictation(&mut self) {
        let ignore_upto = self.last_utterance;
        if let Some(d) = self.dictation.as_mut() {
            d.listening = Some(Instant::now());
            d.ignore_upto = ignore_upto;
        }
        // Новая фраза: недослушанное до сих пор (хвост «запиши идею») — не текст.
        self.pipeline.set_transcribing(false);
        self.pipeline.set_transcribing(true);
        self.apply(Event::Asked);
        tracing::info!("диктовка — слушаю");
        if !self.speak("Слушаю, сэр.".into(), Purpose::Notice) {
            self.play(Reply::Wake);
        }
    }

    /// Фраза диктовки: копится; пока человек говорит — запись откладывается.
    fn on_dictation(&mut self, t: &Transcript, now: Instant) {
        let Some(d) = self.dictation.as_mut() else {
            return;
        };
        if d.listening.is_none() || d.ignore_upto.is_some_and(|n| t.utterance <= n) {
            return;
        }
        let text = t.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        d.heard_at = Some(now);
        if t.is_final {
            d.parts.push(text.clone());
            d.commit_at = Some(now + DICTATION_PAUSE);
        } else {
            d.commit_at = None;
        }
        self.send_ui(UiMsg::Heard(text));
        self.apply(Event::UserSpeaking);
    }

    /// Диктовка кончилась — дописать строку с датой и временем в заметку
    /// (в своём потоке).
    fn commit_dictation(&mut self) {
        let Some(d) = self.dictation.take() else {
            return;
        };
        let text = d.parts.join(" ");
        if text.trim().is_empty() {
            return;
        }
        let line = format!(
            "{}{} — {}",
            d.target.prefix,
            jarvis_core::journal::note_stamp(),
            text.trim()
        );
        tracing::info!(note = %d.target.title, chars = line.chars().count(), "диктовка — записываю в заметку");
        self.apply(Event::Question);
        let events = self.events.clone();
        let NoteTarget { url, title, .. } = d.target;
        let spawned = std::thread::Builder::new()
            .name("jarvis-note".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    jarvis_platform::keep_append_line(&url, &title, &line)
                })
                .unwrap_or_else(|_| Err("запись завершилась паникой".into()));
                let _ = events.send(CoreMsg::NoteDone(result));
            });
        if let Err(e) = spawned {
            let _ = self.events.send(CoreMsg::NoteDone(Err(format!(
                "не удалось запустить поток: {e}"
            ))));
        }
    }

    fn on_note_done(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                tracing::info!("записал в заметку");
                if !self.speak("Записал, сэр.".into(), Purpose::Notice) {
                    self.play(Reply::Done);
                }
                self.apply(Event::ActionFinished { reply: None });
            }
            Err(e) => {
                tracing::warn!(error = %e, "не записал в заметку");
                self.apply(Event::ActionFinished {
                    reply: Some(Reply::Failed),
                });
            }
        }
    }

    /// Команда спросила: ждать ответ (как команду после «Джарвис»), затем
    /// задать вопрос — время вопроса продлит ожидание (`flush_spoke`).
    fn ask_question(&mut self, question: &Question) {
        tracing::info!(choices = question.choices.len(), "вопрос — жду ответ");
        self.apply(Event::Asked);
        let nlu = &self.config.nlu;
        self.choice = Some(Choice::new(
            question,
            Thresholds {
                min_score: nlu.min_score,
                ambiguity_margin: nlu.ambiguity_margin,
            },
        ));
        self.play_sound(&Sound::File(question.sound.clone()));
    }

    // ---------- нейросеть ----------

    /// Отдать ли фразу нейросети: она включена, фраза прозвучала после
    /// «Джарвис» (или сразу после её ответа) и в ней достаточно слов.
    fn wants_llm(&self, text: &str) -> bool {
        let after_wake = matches!(self.state, State::Listening { .. })
            || (self.llm_follow_up && matches!(self.state, State::FollowUp { .. }));
        let words = self.resolver.strip_wake(text).split_whitespace().count();
        self.brain.is_some() && after_wake && words >= self.config.llm.min_words as usize
    }

    fn ask_llm(&mut self, text: &str, now: Instant) {
        let question = self.resolver.strip_wake(text);
        let Some(brain) = &self.brain else {
            return;
        };
        self.llm_generation += 1;
        let generation = self.llm_generation;
        tracing::info!(%question, "вопрос нейросети");
        brain.ask(generation, question.clone());
        if let Some(speech) = &self.speech {
            speech.prepare();
        }
        self.show_answer_text(&question, "", AnswerStatus::Thinking);
        self.answer = Some(AnswerTurn {
            generation,
            question,
            asked: now,
            queue: SpeechQueue::default(),
            next_sentence: SENTENCE_ID_BASE,
            spoken: false,
            cue_played: false,
            done: false,
            failure: None,
            commands: Vec::new(),
        });
        self.apply(Event::Question);
        // Ответ бывает длинным — «стоп» должен слышаться и пока Джарвис говорит.
        self.pipeline.set_barge_in(true);
    }

    fn on_brain(&mut self, event: BrainEvent) {
        let current = |generation: u64, answer: &Option<AnswerTurn>| {
            answer.as_ref().is_some_and(|a| a.generation == generation)
        };
        match event {
            BrainEvent::Sentence { generation, text } if current(generation, &self.answer) => {
                let Some(speech) = self.speech.as_ref().filter(|_| self.speech_ready) else {
                    tracing::warn!("голоса синтеза нет — ответ только текстом в журнале");
                    return;
                };
                if let Some(answer) = self.answer.as_mut() {
                    answer.next_sentence += 1;
                    answer.queue.expect(answer.next_sentence);
                    speech.say_sentence(answer.next_sentence, text);
                }
            }
            BrainEvent::Text { generation, text } if current(generation, &self.answer) => {
                if let Some(question) = self.answer.as_ref().map(|a| a.question.clone()) {
                    self.show_answer_text(&question, &text, AnswerStatus::Answering);
                }
            }
            BrainEvent::Done {
                generation,
                commands,
            } if current(generation, &self.answer) => {
                if let Some(answer) = self.answer.as_mut() {
                    answer.done = true;
                    answer.commands = commands;
                }
            }
            BrainEvent::Failed {
                generation,
                failure,
            } if current(generation, &self.answer) => {
                tracing::warn!(?failure, "нейросеть не ответила");
                if let Some(question) = self.answer.as_ref().map(|a| a.question.clone()) {
                    self.show_answer_text(&question, &failure_text(&failure), AnswerStatus::Failed);
                }
                self.say_failure(&failure);
                if let Some(answer) = self.answer.as_mut() {
                    answer.done = true;
                    answer.failure = Some(failure);
                }
            }
            _ => {} // устаревший ответ
        }
    }

    /// Что сказать, если ответа нет.
    fn say_failure(&mut self, failure: &Failure) {
        match failure {
            Failure::Offline => self.play_in_answer(Reply::Offline),
            Failure::Other(_) => self.play_in_answer(Reply::Failed),
            _ => {
                if !self.speak(failure_text(failure), Purpose::Notice) {
                    self.play_in_answer(Reply::Failed);
                }
            }
        }
    }

    /// Показать (или обновить) окно с вопросом и ответом.
    fn show_answer_text(&mut self, question: &str, answer: &str, status: AnswerStatus) {
        if !self.config.llm.show_window {
            return;
        }
        let view = AnswerView {
            question: question.to_owned(),
            answer: answer.to_owned(),
            status,
        };
        self.text_window_close_at = None;
        match &self.text_window {
            Some(window) if window.is_open() => window.set(view),
            _ => match jarvis_platform::show_answer_window(view, Some(crate::tray::answer_logo)) {
                Ok(window) => self.text_window = Some(window),
                Err(e) => tracing::warn!(error = %e, "окно ответа не открылось"),
            },
        }
    }

    /// Окно ответа закроется само через `llm.window_secs`.
    fn close_answer_text_later(&mut self) {
        if self.text_window.is_some() {
            self.text_window_close_at =
                Some(Instant::now() + Duration::from_secs(u64::from(self.config.llm.window_secs)));
        }
    }

    /// Ответ из пакета внутри ответа нейросети: следующее предложение
    /// подождёт, пока он прозвучит.
    fn play_in_answer(&mut self, reply: Reply) {
        let reply = match reply.fallback() {
            Some(fallback) if self.pack.variants(reply).is_empty() => fallback,
            _ => reply,
        };
        if self.pack.variants(reply).is_empty() {
            return;
        }
        if let Some(duration) = self.player.play(reply) {
            self.muted_for(duration);
            if let Some(answer) = self.answer.as_mut() {
                answer.queue.playing(Instant::now(), duration);
                answer.spoken = true;
            }
        }
    }

    /// Такт ответа: «Секунду, сэр», следующее предложение, конец ответа.
    fn tick_answer(&mut self, now: Instant) {
        let Some(answer) = self.answer.as_ref() else {
            return;
        };
        if !answer.done && now.duration_since(answer.asked) > ANSWER_DEADLINE {
            self.on_brain(BrainEvent::Failed {
                generation: answer.generation,
                failure: Failure::Other("нейросеть не ответила вовремя".into()),
            });
            return;
        }
        if !answer.cue_played && thinking_cue_due(answer.asked, now, answer.spoken, answer.done) {
            if let Some(answer) = self.answer.as_mut() {
                answer.cue_played = true;
            }
            self.play_in_answer(Reply::Thinking);
        }
        let Some(answer) = self.answer.as_mut() else {
            return;
        };
        if let Some(clip) = answer.queue.next(now) {
            match self.player.play_clip(&clip) {
                Ok(duration) => {
                    self.gate.mute_for(duration + ECHO_TAIL);
                    self.spoke_pending += duration + ECHO_TAIL;
                    answer.queue.playing(now, duration);
                    if !std::mem::replace(&mut answer.spoken, true) {
                        tracing::info!(
                            ms = now.duration_since(answer.asked).as_millis() as u64,
                            "ответ нейросети зазвучал"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(error = %jarvis_core::error_chain(&e), "не удалось проиграть ответ")
                }
            }
        }
        if answer.done && answer.queue.idle(now) {
            self.finish_answer();
        }
    }

    fn finish_answer(&mut self) {
        let Some(answer) = self.answer.take() else {
            return;
        };
        let error = answer.failure.as_ref().map(|f| format!("{f:?}"));
        self.journal_answer(&answer, error);
        self.pipeline.set_barge_in(false);
        // Ответ договорён — бегущая полоса в окне останавливается.
        if answer.failure.is_none()
            && let Some(window) = &self.text_window
        {
            window.set_status(AnswerStatus::Done);
        }
        self.close_answer_text_later();
        if answer.failure.is_none() {
            self.llm_follow_up = true;
            self.llm_commands.extend(answer.commands);
        }
        self.apply(Event::ActionFinished { reply: None });
    }

    fn journal_answer(&self, answer: &AnswerTurn, error: Option<String>) {
        let approval = Approval {
            heard: Heard {
                text: answer.question.clone(),
                score: 1.0,
                slots: SlotValues::new(),
                at: answer.asked,
            },
            note: PermissionNote::Builtin,
            answered_by: None,
        };
        let result = if error.is_some() { "failed" } else { "ok" };
        self.write_journal(&approval, LLM_JOURNAL_ID, result, error);
    }

    /// Следующая команда, которую попросила нейросеть, — когда Джарвис свободен.
    fn next_llm_command(&mut self, now: Instant) {
        let free = self.answer.is_none()
            && self.executing.is_none()
            && self.pending.is_none()
            && self.state.accepts_commands();
        if !free {
            return;
        }
        let Some(request) = self.llm_commands.pop_front() else {
            return;
        };
        let checked = self
            .registry
            .get(&request.command)
            .ok_or_else(|| "такой команды больше нет".to_owned())
            .and_then(|c| slot_values(c, request.value.as_deref()));
        match checked {
            Ok(slots) => {
                tracing::info!(command = %request.command, "команда по просьбе нейросети");
                *self.llm_tool.borrow_mut() = Some(request.tool_use_id);
                let text = format!(
                    "нейросеть: {}",
                    spoken_phrase(&self.registry, &request.command)
                );
                self.command(request.command, text, 1.0, slots, now);
            }
            Err(why) => {
                tracing::warn!(command = %request.command, %why, "команда нейросети отклонена");
                if let Some(brain) = &self.brain {
                    brain.outcome(ToolOutcome {
                        tool_use_id: request.tool_use_id,
                        text: format!("Не выполнено: {why}."),
                        is_error: true,
                    });
                }
            }
        }
    }

    // ---------- аварийная остановка и прочее ----------

    fn emergency_stop(&mut self, stop: Stop) {
        tracing::warn!(source = stop.source(), "аварийная остановка");
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
        self.apply(match stop {
            Stop::Hotkey => Event::EmergencyStop,
            Stop::Voice => Event::VoiceStop,
        });
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
        if let Some(brain) = &self.brain {
            brain.set_commands(tool_commands(&registry));
        }
        self.registry = registry;
        true
    }

    fn send_window_commands(&self) {
        let list = crate::main_window::commands_json(&self.registry, &self.permissions);
        self.send_ui(UiMsg::Commands(list));
    }

    /// Решение пользователя из главного окна — как ответ на вопрос-разрешение.
    fn set_permission(&mut self, id: &str, value: &str) {
        let Some(command) = self.registry.get(id) else {
            tracing::warn!(%id, "окно: нет такой команды");
            return;
        };
        let result = match value {
            "ask" => self.permissions.forget(command).map(|_| ()),
            "always" => self.permissions.record(command, Answer::Always),
            "never" => self.permissions.record(command, Answer::Never),
            _ => {
                tracing::warn!(%id, %value, "окно: непонятное разрешение");
                return;
            }
        };
        match result {
            Ok(()) => tracing::info!(%id, %value, "разрешение изменено в окне"),
            Err(e) => {
                tracing::error!(error = %jarvis_core::error_chain(&e), %id, "разрешение не сохранилось")
            }
        }
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
        // Команду просила нейросеть — сообщить ей итог.
        if command != LLM_JOURNAL_ID
            && let Some(tool_use_id) = self.llm_tool.borrow_mut().take()
            && let Some(brain) = &self.brain
        {
            brain.outcome(ToolOutcome {
                tool_use_id,
                text: match result {
                    "ok" => "Выполнено.".to_owned(),
                    "not_executed" => "Не выполнено: пользователь не разрешил.".to_owned(),
                    _ => format!(
                        "Не удалось{}.",
                        error
                            .as_deref()
                            .map(|e| format!(": {e}"))
                            .unwrap_or_default()
                    ),
                },
                is_error: result != "ok",
            });
        }
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
        // Старый пакет может не знать нового ответа — сказать близкий по смыслу.
        let reply = match reply.fallback() {
            Some(fallback) if self.pack.variants(reply).is_empty() => fallback,
            _ => reply,
        };
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
                // Предложение ответа нейросети — в очередь, звучит по порядку.
                if let Some(answer) = self.answer.as_mut()
                    && answer.queue.ready(id, Arc::clone(&clip))
                {
                    return;
                }
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
                if let Some(answer) = self.answer.as_mut()
                    && answer.queue.contains(id)
                {
                    tracing::warn!(%why, "предложение ответа не озвучено");
                    answer.queue.failed(id);
                    return;
                }
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
                    // Звук оборван — микрофон глушится только на хвост эха,
                    // а не до конца прерванной фразы: сразу слышно «Джарвис».
                    self.gate.cut_to(ECHO_TAIL);
                    self.pipeline.set_barge_in(false);
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
                    // Ответ нейросети — прервать, недоговорённое и её команды — забыть.
                    if let Some(brain) = &self.brain {
                        brain.cancel();
                    }
                    if let Some(answer) = self.answer.take() {
                        self.journal_answer(&answer, Some("аварийная остановка".into()));
                    }
                    if let Some(window) = self.text_window.take() {
                        window.close();
                    }
                    self.llm_commands.clear();
                    self.dictation = None;
                }
            }
        }
        if matches!(self.state, State::Idle | State::Paused) {
            self.llm_follow_up = false;
        }
        // Режим диалога кончился (выход, тайм-аут, стоп, пауза) — забыть его.
        if matches!(self.state, State::Idle | State::Paused) && self.timings.dialog.is_some() {
            tracing::info!("режим диалога выключен");
            self.timings.dialog = None;
        }
        // Диктовка — только пока ждём текст; ушли из ожидания (тишина, стоп,
        // пауза) — недиктованное забыто.
        if self
            .dictation
            .as_ref()
            .is_some_and(|d| d.listening.is_some())
            && !matches!(self.state, State::Listening { .. })
        {
            tracing::info!("диктовка прервана");
            self.dictation = None;
        }
        // Вопрос команды действует, пока ждём ответ; новое «Джарвис» — новый разговор.
        if !matches!(self.state, State::Listening { .. }) || event == Event::WakeWord {
            self.choice = None;
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
            (
                State::Listening { .. }
                | State::Executing
                | State::FollowUp { .. }
                | State::Conversation { .. },
                None,
                true,
            ) => Indicator::Listening,
        };
        self.send_ui(UiMsg::Indicator(indicator));
    }

    fn send_ui(&self, msg: UiMsg) {
        if self.ui.send(msg).is_ok() {
            self.waker.wake();
        }
    }
}

/// Что сказать и показать, если нейросеть не ответила.
fn failure_text(failure: &Failure) -> String {
    match failure {
        Failure::NoKey => "Сэр, ключ доступа к нейросети не настроен.".into(),
        Failure::BadKey => "Сэр, сервер не принял ключ доступа к нейросети.".into(),
        Failure::Busy => "Сервер нейросети перегружен, сэр. Попробуйте чуть позже.".into(),
        Failure::Refused => "Боюсь, на это я не отвечу, сэр.".into(),
        Failure::Offline => "Нет связи с сервером, сэр.".into(),
        Failure::Other(why) => format!("Не удалось, сэр ({why})."),
    }
}

/// Имя «команды» вопроса нейросети в журнале действий.
const LLM_JOURNAL_ID: &str = "llm";

/// Поток нейросети: ключ — из хранилища ОС при первом вопросе.
fn spawn_brain(config: &Config, registry: &Registry, events: Sender<CoreMsg>) -> Option<Brain> {
    let llm = &config.llm;
    let settings = ClaudeSettings {
        model: llm.model.clone(),
        effort: llm.effort.clone(),
        max_tokens: llm.max_tokens,
        thinking: llm.thinking.clone(),
        fallback: llm.fallback,
        bearer: llm.auth == "bearer",
    };
    let url = messages_url(&llm.server);
    let factory: ModelFactory = Box::new(move || {
        let key = jarvis_llm::secrets::api_key()?.ok_or(LlmError::NoKey)?;
        let client = ClaudeClient::with_url(key, settings.clone(), url.clone())?;
        Ok(Box::new(client) as Box<dyn LanguageModel>)
    });
    let memory = Duration::from_secs(u64::from(llm.memory_mins) * 60);
    tracing::info!(model = %llm.model, effort = %llm.effort, server = %llm.server, "нейросеть включена");
    Brain::spawn(
        factory,
        tool_commands(registry),
        memory,
        Box::new(move |event| {
            let _ = events.send(CoreMsg::Brain(event));
        }),
    )
    .inspect_err(|e| tracing::error!(error = %e, "не удалось запустить поток нейросети"))
    .ok()
}

/// Текст окна-вопроса: что именно произойдёт (раздел 2.1 ТЗ) — с уже
/// подставленными значениями слотов.
fn permission_dialog(command: &Command, offer_always: bool, slots: &SlotValues) -> DialogSpec {
    let phrase = speech::filled_phrase(command, slots);
    let description = jarvis_core::slots::fill(&command.description, slots);
    let action = command.action.with_slots(slots);
    let details = match &action {
        ActionSpec::OpenUrl { url, .. } => format!("Адрес: {url}"),
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
                    find_tab: Vec::new(),
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

    #[test]
    fn answer_to_question_picks_command() {
        let question = Question {
            sound: PathBuf::from("ask.wav"),
            choices: vec![
                ("кс".into(), "game_cs2".into()),
                ("контра".into(), "game_cs2".into()),
                ("контр страйк".into(), "game_cs2".into()),
                ("дота".into(), "game_dota".into()),
            ],
        };
        let choice = Choice::new(&question, Thresholds::default());
        assert_eq!(choice.ids, ["game_cs2", "game_dota"]);
        let id = |text: &str| choice.resolve(text).map(|(id, _)| id);
        assert_eq!(id("кс").as_deref(), Some("game_cs2"));
        assert_eq!(id("контр страйк").as_deref(), Some("game_cs2"));
        assert_eq!(id("ну дота").as_deref(), Some("game_dota"));
        assert_eq!(id("купи хлеба"), None);
    }
}
