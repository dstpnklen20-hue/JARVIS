//! Демон: трей и горячие клавиши в главном потоке, ядро ([`crate::core`]) —
//! в потоке `jarvis-core`, слух — в потоке `jarvis-listen`, каждое
//! действие — в своём коротком потоке `jarvis-action`, окно-вопрос — в
//! потоке `jarvis-ask`.
//!
//! ```text
//!  jarvis-listen ──PipelineEvent──▶ jarvis-core ──Indicator──▶ главный поток (трей)
//!        ▲                          │  ▲    │                       │
//!        └─ микрофон / распознавание┘  │    └─▶ jarvis-action ─┐    │
//!                                      ├──── результат ────────┘    │
//!                                      ├──── кнопка окна (jarvis-ask)
//!                                      └─ Пауза / Стоп / Забыть ────┘
//! ```

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context as _;
use jarvis_app::resolver::{CommandResolver, hotword_phrases};
use jarvis_audio::asr::Hotwords;
use jarvis_audio::pack::VoicePack;
use jarvis_audio::pipeline::PipelineSettings;
use jarvis_core::Config;
use jarvis_core::commands::Registry;
use jarvis_core::config::resolve;
use jarvis_core::journal::ActionJournal;
use jarvis_core::permissions::{PERMISSIONS_FILE, PermissionStore};
use jarvis_core::root::project_path;
use jarvis_platform::LoopEvent;

use crate::core::{Core, CoreMsg, CoreParams, VoiceSetup};
use crate::hearing;
use crate::tray::{Indicator, MenuAction, Tray};

pub const INSTANCE_NAME: &str = "JarvisVoiceAssistant";
/// Страховочный такт цикла сообщений (см. jarvis_platform::run_message_loop).
const UI_TICK: Duration = Duration::from_millis(250);
const HOTKEY_EMERGENCY_STOP: i32 = 1;

pub fn run(root: &Path, config: Config) -> anyhow::Result<()> {
    let _instance = jarvis_platform::acquire_single_instance(INSTANCE_NAME)?
        .context("Джарвис уже запущен — его значок в трее")?;

    // Битый файл разрешений — не запускаемся: иначе первая же запись
    // разрешения затёрла бы его содержимое.
    let permissions = PermissionStore::load(&project_path(root, PERMISSIONS_FILE))
        .context("файл разрешений не читается — исправьте или удалите его")?;
    tracing::info!(
        remembered = permissions.remembered(),
        "разрешения загружены"
    );
    if !config.hotkeys.emergency_stop.trim().is_empty() {
        jarvis_platform::parse_hotkey(&config.hotkeys.emergency_stop)
            .map_err(|e| anyhow::anyhow!("hotkeys.emergency_stop: {e}"))?;
    }

    let hearing = hearing::prepare(root, &config)?;
    let pack_dir = resolve(root, &config.paths.sounds).join(&config.voice.pack);
    let pack = VoicePack::load(&pack_dir).context("звуковой пакет не загружен")?;
    let registry = load_commands(root, &config);
    let journal = match ActionJournal::open(
        &resolve(root, &config.logging.dir),
        config.logging.max_files,
    ) {
        Ok(j) => Some(j),
        Err(e) => {
            tracing::error!(error = %e, "журнал действий не открылся — работаю без него");
            None
        }
    };

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
    let mut tray = Tray::new(autostart::enabled())?;

    // Горячая клавиша аварийной остановки — в этом (главном) потоке.
    let _hotkey = match config.hotkeys.emergency_stop.trim() {
        "" => None,
        spec => match jarvis_platform::register_hotkey(HOTKEY_EMERGENCY_STOP, spec) {
            Ok(guard) => {
                tracing::info!(hotkey = spec, "аварийная остановка — горячая клавиша");
                Some(guard)
            }
            Err(e) => {
                tracing::warn!(error = %e, "горячая клавиша аварийной остановки не работает; остаётся «Джарвис, стоп»");
                None
            }
        },
    };

    let (core_tx, core_rx) = mpsc::channel::<CoreMsg>();
    let (ui_tx, ui_rx) = mpsc::channel::<Indicator>();
    let commands_dir = resolve(root, &config.paths.commands);
    let wake_matcher = hearing.matcher.clone();
    let voice = voice_setup(root, &config);
    let elevated = jarvis_platform::is_elevated().unwrap_or(false);
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
        permissions,
        journal,
        config,
        commands_dir,
        wake_matcher,
        voice,
        elevated,
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
    jarvis_platform::run_message_loop(UI_TICK, |event| {
        if event == LoopEvent::Hotkey(HOTKEY_EMERGENCY_STOP) {
            let _ = core_tx.send(CoreMsg::EmergencyStop);
        }
        while let Ok(indicator) = ui_rx.try_recv() {
            tray.show(indicator);
        }
        match tray.poll_menu() {
            Some(MenuAction::TogglePause) => {
                let _ = core_tx.send(CoreMsg::TogglePause);
            }
            Some(MenuAction::ForgetPermissions) => {
                let _ = core_tx.send(CoreMsg::ForgetPermissions);
            }
            Some(MenuAction::ReloadCommands) => {
                let _ = core_tx.send(CoreMsg::ReloadCommands);
            }
            Some(MenuAction::ToggleAutostart) => tray.set_autostart(autostart::toggle()),
            Some(MenuAction::Quit) => return ControlFlow::Break(()),
            None => {}
        }
        ControlFlow::Continue(())
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

/// Голос синтеза из настроек. Без него Джарвис работает — динамические
/// ответы заменяются фразами звукового пакета.
fn voice_setup(root: &Path, config: &Config) -> Option<VoiceSetup> {
    let name = config.voice.tts_voice()?;
    match jarvis_audio::models::tts_voice(root, config, name) {
        Ok(model) => Some(VoiceSetup {
            name: name.to_owned(),
            model,
            speed: config.voice.tts_speed,
        }),
        Err(e) => {
            tracing::warn!(
                voice = name,
                error = %jarvis_core::error_chain(&e),
                "голос синтеза не найден — отвечаю звуковым пакетом"
            );
            None
        }
    }
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
    if registry.commands.is_empty() && registry.errors.is_empty() {
        tracing::warn!(dir = %dir.display(), "ни одной команды — Джарвис будет только откликаться");
    }
    registry
}

/// Проверка горячей клавиши для `--check`.
pub fn check_hotkey(config: &Config) -> Result<Option<String>, String> {
    match config.hotkeys.emergency_stop.trim() {
        "" => Ok(None),
        spec => jarvis_platform::parse_hotkey(spec).map(|_| Some(spec.to_owned())),
    }
}

/// Автозапуск при входе в Windows: `jarvisw.exe` рядом с `jarvis.exe`
/// (без окна консоли).
mod autostart {
    const NAME: &str = "Jarvis";

    pub fn enabled() -> bool {
        jarvis_platform::autostart_command(NAME).is_ok_and(|c| c.is_some())
    }

    /// Переключить; вернуть, включён ли автозапуск теперь.
    pub fn toggle() -> bool {
        let result = if enabled() {
            jarvis_platform::set_autostart(NAME, None)
        } else {
            match std::env::current_exe() {
                Ok(exe) => {
                    let command = format!("\"{}\"", exe.with_file_name("jarvisw.exe").display());
                    jarvis_platform::set_autostart(NAME, Some(&command))
                }
                Err(e) => {
                    tracing::error!(error = %e, "не найден путь программы");
                    return enabled();
                }
            }
        };
        if let Err(e) = result {
            tracing::error!(error = %e, "не удалось изменить автозапуск");
        }
        let now = enabled();
        tracing::info!(on = now, "автозапуск при входе в Windows");
        now
    }
}
