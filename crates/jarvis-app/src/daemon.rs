//! Демон: трей в главном потоке, ядро (автомат состояний, звук ответа) —
//! в потоке `jarvis-core`, слух — в потоке `jarvis-listen`.
//!
//! ```text
//!  jarvis-listen ──PipelineEvent──▶ jarvis-core ──Indicator──▶ главный поток (трей)
//!        ▲                            │   ▲                         │
//!        └──── вкл/выкл микрофон ─────┘   └──── Пауза / Выход ──────┘
//! ```

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_audio::pack::VoicePack;
use jarvis_audio::pipeline::{MuteGate, Pipeline, PipelineEvent, PipelineSettings, WakeEvent};
use jarvis_audio::player::Player;
use jarvis_core::Config;
use jarvis_core::config::resolve;
use jarvis_core::state::{Effect, Event, State, Timings, transition};
use jarvis_platform::Waker;

use crate::hearing;
use crate::tray::{Indicator, MenuAction, Tray};

pub const INSTANCE_NAME: &str = "JarvisVoiceAssistant";
/// Как часто ядро проверяет таймауты.
const CORE_TICK: Duration = Duration::from_millis(100);
/// Страховочный такт цикла сообщений (см. jarvis_platform::run_message_loop).
const UI_TICK: Duration = Duration::from_millis(250);
/// После своей фразы ещё немного не слушаем: эхо комнаты.
const ECHO_TAIL: Duration = Duration::from_millis(150);

enum CoreMsg {
    Pipeline(PipelineEvent),
    TogglePause,
    Shutdown,
}

pub fn run(root: &Path, config: Config) -> anyhow::Result<()> {
    let _instance = jarvis_platform::acquire_single_instance(INSTANCE_NAME)?
        .context("Джарвис уже запущен — его значок в трее")?;

    let hearing = hearing::prepare(root, &config)?;
    let pack_dir = resolve(root, &config.paths.sounds).join(&config.voice.pack);
    let pack = VoicePack::load(&pack_dir).context("звуковой пакет не загружен")?;

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
    let settings = PipelineSettings {
        device: config.audio.input_device_name().map(str::to_owned),
        models: hearing.models,
        matcher: hearing.matcher,
        voiceprint: hearing.voiceprint,
    };
    let timings = Timings {
        listen: Duration::from_secs(u64::from(config.wake.listen_secs)),
    };
    let volume = config.voice.volume;

    let core_events = core_tx.clone();
    let core = std::thread::Builder::new()
        .name("jarvis-core".into())
        .spawn(move || {
            let core = Core::start(settings, core_events, pack, volume, ui_tx, waker, timings);
            match core {
                Ok(mut core) => core.run(core_rx),
                Err(e) => {
                    tracing::error!(error = %jarvis_core::error_chain(e.as_ref()), "ядро не запустилось");
                    waker.quit();
                }
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

/// Ядро: автомат состояний и исполнение его эффектов.
struct Core {
    state: State,
    timings: Timings,
    pipeline: Pipeline,
    player: Player,
    gate: Arc<MuteGate>,
    ui: Sender<Indicator>,
    waker: Waker,
    /// Слух сообщил о проблеме — показываем её, пока не наладится.
    problem: Option<String>,
    ready: bool,
}

impl Core {
    fn start(
        settings: PipelineSettings,
        events: Sender<CoreMsg>,
        pack: VoicePack,
        volume: f32,
        ui: Sender<Indicator>,
        waker: Waker,
        timings: Timings,
    ) -> anyhow::Result<Self> {
        // Player создаётся здесь: поток вывода звука нельзя передавать между потоками.
        let player = Player::new(&pack, volume).context("вывод звука не открылся")?;
        let gate = Arc::new(MuteGate::default());
        let pipeline = Pipeline::spawn(
            settings,
            Box::new(move |event| {
                let _ = events.send(CoreMsg::Pipeline(event));
            }),
            Arc::clone(&gate),
        )
        .context("не удалось запустить поток слуха")?;
        Ok(Self {
            state: State::Idle,
            timings,
            pipeline,
            player,
            gate,
            ui,
            waker,
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
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.apply(Event::Tick);
        }
        self.player.stop();
        // Pipeline при удалении останавливает поток и освобождает микрофон.
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
        let before = self.state;
        self.apply(Event::WakeWord);
        if matches!(before, State::Idle) {
            tracing::info!(
                heard = %wake.heard,
                voice = ?wake.similarity,
                verify_ms = wake.verify_time.as_millis() as u64,
                reply_ms = wake.detected_at.elapsed().as_millis() as u64,
                "«Джарвис» — отвечаю"
            );
        }
    }

    fn apply(&mut self, event: Event) {
        let before = self.state;
        let tr = transition(self.state, event, Instant::now(), self.timings);
        for effect in tr.effects {
            match effect {
                Effect::PlayReply(reply) => {
                    if let Some(duration) = self.player.play(reply) {
                        self.gate.mute_for(duration + ECHO_TAIL);
                    }
                }
                Effect::SetListening(on) => self.pipeline.set_listening(on),
            }
        }
        self.state = tr.next;
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
            (State::Listening { .. }, None, true) => Indicator::Listening,
        };
        if self.ui.send(indicator).is_ok() {
            self.waker.wake();
        }
    }
}
