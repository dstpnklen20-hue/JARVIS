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
use std::time::{Duration, Instant};

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
use serde_json::json;

use crate::core::{Core, CoreMsg, CoreParams, VoiceSetup};
use crate::hearing;
use crate::main_window::{MainWindow, PageMsg, WindowModel};
use crate::settings::{self, SettingsForm};
use crate::tray::{MenuAction, Tray, UiMsg};

pub const INSTANCE_NAME: &str = "JarvisVoiceAssistant";
/// Страховочный такт цикла сообщений (см. jarvis_platform::run_message_loop).
const UI_TICK: Duration = Duration::from_millis(250);
const HOTKEY_EMERGENCY_STOP: i32 = 1;
/// Сколько ждать завершения прежней копии при перезапуске.
const RESTART_WAIT: Duration = Duration::from_secs(20);

/// `restart` — запуск из `--restart`: прежняя копия уже получила команду
/// остановки, подождать, пока она завершится.
pub fn run(root: &Path, config: Config, restart: bool) -> anyhow::Result<()> {
    let mut instance = jarvis_platform::acquire_single_instance(INSTANCE_NAME)?;
    let deadline = Instant::now() + RESTART_WAIT;
    while instance.is_none() && restart && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        instance = jarvis_platform::acquire_single_instance(INSTANCE_NAME)?;
    }
    let Some(_instance) = instance else {
        // Уже работает — открыть его окно (ярлык «Джарвис» при запущенном).
        if jarvis_platform::send_show_signal(INSTANCE_NAME).unwrap_or(false) {
            tracing::info!("Джарвис уже запущен — открываю его окно");
            return Ok(());
        }
        anyhow::bail!("Джарвис уже запущен — его значок в трее");
    };

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
    // Главное окно — в настоящем разрешении экрана (до создания окон потока).
    jarvis_platform::use_per_monitor_dpi();
    let mut tray = Tray::new(autostart::enabled())?;
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    // Повторный запуск Джарвиса — показать окно.
    let show = jarvis_platform::create_show_signal(INSTANCE_NAME)?;
    let show_tx = ui_tx.clone();
    std::thread::Builder::new()
        .name("jarvis-show".into())
        .spawn(move || {
            while show.wait().is_ok() {
                if show_tx.send(UiMsg::ShowWindow).is_err() {
                    break;
                }
                waker.wake();
            }
        })
        .context("не удалось запустить поток показа окна")?;

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
    let window_config = config.clone();
    let webview_dir = resolve(root, &config.paths.data).join("webview");
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
    let mut model = WindowModel::default();
    let mut window: Option<MainWindow> = None;
    jarvis_platform::run_message_loop(UI_TICK, |event| {
        if event == LoopEvent::Hotkey(HOTKEY_EMERGENCY_STOP) {
            let _ = core_tx.send(CoreMsg::EmergencyStop);
        }
        let mut show_window = false;
        while let Ok(msg) = ui_rx.try_recv() {
            match msg {
                UiMsg::ShowWindow => show_window = true,
                UiMsg::Commands(list) => {
                    if let Some(w) = &window {
                        w.update(&list);
                    }
                }
                UiMsg::Indicator(indicator) => {
                    tray.show(indicator.clone());
                    model.indicator = indicator;
                    if let Some(w) = &window {
                        w.update(&model.state_update());
                    }
                }
                UiMsg::Heard(text) => {
                    if let Some(w) = &window {
                        w.update(&json!({ "heard": text }));
                    }
                    model.heard = text;
                }
                UiMsg::Mic(name) => {
                    model.mic = Some(name);
                    if let Some(w) = &window {
                        w.update(&model.state_update());
                    }
                }
            }
        }
        if window.as_ref().is_some_and(|w| !w.is_open()) {
            window = None; // память WebView2 освобождается вместе с окном
            tracing::info!("главное окно закрыто");
        }
        if let Some(w) = window.as_mut() {
            for msg in w.take_messages() {
                tracing::debug!(kind = msg.kind(), "сообщение окна");
                match msg {
                    PageMsg::Ready => {
                        w.update(&model.snapshot(&window_config));
                        w.update(&settings::settings_json(
                            root,
                            &window_config,
                            &settings::input_devices(),
                            autostart::enabled(),
                        ));
                    }
                    PageMsg::Autostart { on } => tray.set_autostart(autostart::set(on)),
                    PageMsg::ApiKey { key } => {
                        let note = save_api_key(&key);
                        w.update(&json!({ "note": note, "status": { "ai": crate::main_window::ai_status(&window_config) } }));
                    }
                    PageMsg::Commands => {
                        let _ = core_tx.send(CoreMsg::WindowCommands);
                    }
                    PageMsg::Permission { id, value } => {
                        let _ = core_tx.send(CoreMsg::SetPermission { id, value });
                    }
                    PageMsg::Settings(values) => {
                        let note = save_settings(root, &window_config, values);
                        w.update(&note);
                    }
                    PageMsg::Restart => match spawn_restart() {
                        Ok(()) => tracing::info!("перезапуск из окна настроек"),
                        Err(e) => {
                            tracing::error!(error = %e, "перезапуск не удался");
                            w.update(
                                &json!({ "note": "Перезапуск не удался — подробности в логе" }),
                            );
                        }
                    },
                }
            }
            let now = Instant::now();
            if let Some(resources) = model.resources_due(now) {
                w.update(&resources);
            }
            let (mic, output) = (
                jarvis_audio::level::MIC.get(),
                jarvis_audio::level::OUTPUT.get(),
            );
            if let Some(level) = model.level_due(now, mic, output) {
                w.update(&level);
            }
        }
        let action = match tray.poll_menu() {
            None if show_window => Some(MenuAction::OpenWindow),
            action => action,
        };
        match action {
            Some(MenuAction::OpenWindow) => match &window {
                Some(w) => w.bring_to_front(),
                None => match MainWindow::open(&webview_dir) {
                    Ok(w) => {
                        tracing::info!("главное окно открыто");
                        window = Some(w);
                    }
                    Err(e) => tracing::error!(
                        error = %jarvis_core::error_chain(e.as_ref()),
                        "главное окно не открылось"
                    ),
                },
            },
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
    drop(window);
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

/// «Сохранить» в окне настроек: записать user.toml, вернуть ответ окну.
fn save_settings(root: &Path, config: &Config, values: serde_json::Value) -> serde_json::Value {
    let form: SettingsForm = match serde_json::from_value(values) {
        Ok(form) => form,
        Err(e) => {
            tracing::warn!(error = %e, "окно прислало непонятные настройки");
            return json!({ "note": "Не сохранено: окно прислало непонятные данные" });
        }
    };
    match settings::save(root, config, &settings::input_devices(), &form) {
        Ok(changed) if changed.is_empty() => json!({ "note": "Ничего не изменилось" }),
        Ok(changed) => {
            tracing::info!(changed = ?changed, "настройки сохранены из окна");
            json!({
                "note": format!("Сохранено: {}. Применится после перезапуска", changed.join(", ")),
                "restart": true,
            })
        }
        Err(e) => {
            let why = jarvis_core::error_chain(&e);
            tracing::warn!(error = %why, "настройки не сохранены");
            json!({ "note": format!("Не сохранено: {why}") })
        }
    }
}

/// Перезапуск: новая копия (`jarvisw --restart`) остановит эту и запустится.
fn spawn_restart() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe.with_file_name("jarvisw.exe"))
        .arg("--restart")
        .spawn()
        .map(|_| ())
}

/// Ключ API из окна — в хранилище ОС; вернуть текст для окна. Ключ не
/// пишется в лог.
fn save_api_key(key: &str) -> String {
    let key = key.trim();
    if !jarvis_llm::secrets::looks_like_key(key) && !jarvis_llm::secrets::looks_like_server_key(key)
    {
        return "Это не похоже на ключ: только латиница, цифры и знаки, без пробелов".into();
    }
    match jarvis_llm::secrets::save_api_key(key) {
        Ok(()) => {
            tracing::info!("ключ API сохранён из окна");
            "Ключ сохранён в Диспетчере учётных данных Windows".into()
        }
        Err(e) => {
            tracing::error!(error = %jarvis_core::error_chain(&e), "ключ API не сохранился");
            "Ключ не сохранился — подробности в логе".into()
        }
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
        set(!enabled())
    }

    /// Включить или выключить; вернуть, включён ли автозапуск теперь.
    pub fn set(on: bool) -> bool {
        let result = if !on {
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
