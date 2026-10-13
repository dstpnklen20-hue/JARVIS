//! Вкладка «Настройки» главного окна: что показать и как сохранить.
//!
//! Сохранение правит `config/user.toml` через `toml_edit`: ваши комментарии и
//! порядок остаются, пишутся только изменённые значения (остальное и дальше
//! берётся из `default.toml`). Новый файл сначала проверяется целиком
//! (`Config::from_sources`) — сломанный конфиг на диск не попадает.
//! Применяется после перезапуска Джарвиса.

use std::path::Path;

use jarvis_core::Config;
use jarvis_core::config::{
    DEFAULT_FILE, FOLLOW_UP_SECS_RANGE, SYSTEM_DEFAULT_DEVICE, SpeakerThreshold, USER_FILE,
    VOLUME_RANGE, resolve,
};
use jarvis_core::root::project_path;
use serde::Deserialize;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};

/// Значения из окна (кнопка «Сохранить»).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsForm {
    pub mic: String,
    pub pack: String,
    pub tts: String,
    /// Проценты: 100 — без изменений, 0–200.
    pub volume: u32,
    pub llm: bool,
    pub show_window: bool,
    /// Сколько секунд ждать следующую команду без «Джарвис».
    pub follow_up_secs: u32,
    /// Аварийная остановка («Ctrl+Alt+F12»; пусто — без клавиши).
    pub emergency_stop: String,
    /// Команды распознаются и пишутся в журнал, но не выполняются.
    pub dry_run: bool,
    pub greet: bool,
    /// Откликаться только на ваш голос.
    pub voice_check: bool,
    /// Порог сходства голоса: «auto» или число 0.10–0.90.
    pub threshold: String,
}

/// Строгость проверки голоса: варианты для окна.
const THRESHOLDS: [(&str, &str); 6] = [
    ("auto", "Авто (по образцу)"),
    ("0.25", "Мягко — 0.25"),
    ("0.30", "0.30"),
    ("0.35", "0.35"),
    ("0.40", "0.40"),
    ("0.50", "Строго — 0.50"),
];

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("горячая клавиша «{0}» не распознана (пример: Ctrl+Alt+F12)")]
    Hotkey(String),
    #[error("«{value}» нет среди вариантов «{field}»")]
    Unknown { field: &'static str, value: String },
    #[error("не удалось прочитать {path}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("в {USER_FILE} ошибка — исправьте её вручную")]
    Parse(#[source] toml_edit::TomlError),
    #[error("в {USER_FILE} раздел [{0}] записан не таблицей — исправьте вручную")]
    NotTable(&'static str),
    #[error("новые настройки не прошли проверку")]
    Invalid(#[source] jarvis_core::config::ConfigError),
    #[error("не удалось записать {path}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

/// Микрофоны системы (имена cpal).
pub fn input_devices() -> Vec<String> {
    use cpal::traits::HostTrait as _;
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

/// Звуковые пакеты: папка → имя из pack.toml.
pub fn sound_packs(root: &Path, config: &Config) -> Vec<(String, String)> {
    let dir = resolve(root, &config.paths.sounds);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut packs: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let folder = e.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(e.path().join("pack.toml")).ok()?;
            let name = text
                .parse::<toml::Table>()
                .ok()
                .and_then(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                .unwrap_or_else(|| folder.clone());
            Some((folder, name))
        })
        .collect();
    packs.sort();
    packs
}

/// Всё для вкладки «Настройки».
pub fn settings_json(root: &Path, config: &Config, devices: &[String], autostart: bool) -> Value {
    let mut mics =
        vec![json!({ "value": SYSTEM_DEFAULT_DEVICE, "label": "Системный по умолчанию" })];
    mics.extend(devices.iter().map(|d| json!({ "value": d, "label": d })));
    // В конфиге — часть имени: показать устройство, которое ей соответствует.
    let mic = match config.audio.input_device_name() {
        None => SYSTEM_DEFAULT_DEVICE.to_owned(),
        Some(part) => match find_device(devices, part) {
            Some(name) => name.to_owned(),
            None => {
                mics.push(json!({ "value": part, "label": format!("{part} (не подключён)") }));
                part.to_owned()
            }
        },
    };
    let packs: Vec<Value> = sound_packs(root, config)
        .into_iter()
        .map(|(value, label)| json!({ "value": value, "label": label }))
        .collect();
    let mut voices = vec![json!({ "value": "none", "label": "Без синтеза" })];
    voices.extend(
        jarvis_audio::models::installed_voices(root, config)
            .into_iter()
            .map(|name| json!({ "value": name, "label": title_case(&name) })),
    );
    let has_key = matches!(jarvis_llm::secrets::api_key(), Ok(Some(_)));
    json!({ "settings": {
        "mics": mics,
        "mic": mic,
        "packs": packs,
        "pack": config.voice.pack,
        "voices": voices,
        "tts": config.voice.tts,
        "volume": volume_percent(config.voice.volume),
        "llm": config.llm.enabled,
        "show_window": config.llm.show_window,
        "autostart": autostart,
        "has_key": has_key,
        "follow_up_secs": config.session.follow_up_secs,
        "emergency_stop": config.hotkeys.emergency_stop,
        "dry_run": config.general.dry_run,
        "greet": config.general.greet_on_start,
        "voice_check": config.speaker.verify,
        "thresholds": THRESHOLDS
            .iter()
            .map(|(value, label)| json!({ "value": value, "label": label }))
            .collect::<Vec<_>>(),
        "threshold": threshold_text(&config.speaker.threshold),
    }})
}

/// Сохранить изменённое в user.toml; вернуть, что изменилось (для окна).
pub fn save(
    root: &Path,
    config: &Config,
    devices: &[String],
    form: &SettingsForm,
) -> Result<Vec<&'static str>, SettingsError> {
    // Значения — только из предложенных вариантов.
    let mic_ok = form.mic == SYSTEM_DEFAULT_DEVICE
        || devices.contains(&form.mic)
        || config.audio.input_device_name() == Some(form.mic.as_str());
    if !mic_ok {
        return Err(unknown("микрофон", &form.mic));
    }
    if !sound_packs(root, config)
        .iter()
        .any(|(folder, _)| *folder == form.pack)
    {
        return Err(unknown("звуковой пакет", &form.pack));
    }
    let voices = jarvis_audio::models::installed_voices(root, config);
    if form.tts != "none" && !voices.contains(&form.tts) {
        return Err(unknown("голос синтеза", &form.tts));
    }
    let volume = form.volume as f32 / 100.0;
    if !VOLUME_RANGE.contains(&volume) {
        return Err(unknown("громкость", &form.volume.to_string()));
    }
    if !FOLLOW_UP_SECS_RANGE.contains(&form.follow_up_secs) {
        return Err(unknown(
            "ожидание команды",
            &form.follow_up_secs.to_string(),
        ));
    }
    let hotkey = form.emergency_stop.trim();
    if !hotkey.is_empty() && jarvis_platform::parse_hotkey(hotkey).is_err() {
        return Err(SettingsError::Hotkey(hotkey.to_owned()));
    }
    if !THRESHOLDS.iter().any(|(value, _)| *value == form.threshold)
        && threshold_text(&config.speaker.threshold) != form.threshold
    {
        return Err(unknown("строгость проверки голоса", &form.threshold));
    }

    let user_path = project_path(root, USER_FILE);
    let user_text = match std::fs::read_to_string(&user_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(SettingsError::Read {
                path: user_path.display().to_string(),
                source,
            });
        }
    };
    let (text, changed) = apply(&user_text, config, form)?;
    if changed.is_empty() {
        return Ok(changed);
    }

    let default_path = project_path(root, DEFAULT_FILE);
    let default_text =
        std::fs::read_to_string(&default_path).map_err(|source| SettingsError::Read {
            path: default_path.display().to_string(),
            source,
        })?;
    Config::from_sources((&default_path, &default_text), Some((&user_path, &text)))
        .map_err(SettingsError::Invalid)?;

    // Через временный файл: оборванная запись не портит настройки.
    let write_err = |source| SettingsError::Write {
        path: user_path.display().to_string(),
        source,
    };
    let tmp = user_path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(write_err)?;
    std::fs::rename(&tmp, &user_path).map_err(write_err)?;
    Ok(changed)
}

/// Новый текст user.toml и названия изменённых настроек.
fn apply(
    user_text: &str,
    config: &Config,
    form: &SettingsForm,
) -> Result<(String, Vec<&'static str>), SettingsError> {
    let mut doc: DocumentMut = user_text.parse().map_err(SettingsError::Parse)?;
    let mut changed = Vec::new();
    let current_mic = config
        .audio
        .input_device_name()
        .unwrap_or(SYSTEM_DEFAULT_DEVICE);
    if form.mic != current_mic {
        set(&mut doc, "audio", "input_device", form.mic.as_str().into())?;
        changed.push("микрофон");
    }
    if form.pack != config.voice.pack {
        set(&mut doc, "voice", "pack", form.pack.as_str().into())?;
        changed.push("звуковой пакет");
    }
    if form.tts != config.voice.tts {
        set(&mut doc, "voice", "tts", form.tts.as_str().into())?;
        changed.push("голос синтеза");
    }
    if form.volume != volume_percent(config.voice.volume) {
        set(
            &mut doc,
            "voice",
            "volume",
            (f64::from(form.volume) / 100.0).into(),
        )?;
        changed.push("громкость");
    }
    if form.llm != config.llm.enabled {
        set(&mut doc, "llm", "enabled", form.llm.into())?;
        changed.push("нейросеть");
    }
    if form.show_window != config.llm.show_window {
        set(&mut doc, "llm", "show_window", form.show_window.into())?;
        changed.push("окно ответа");
    }
    if form.follow_up_secs != config.session.follow_up_secs {
        set(
            &mut doc,
            "session",
            "follow_up_secs",
            i64::from(form.follow_up_secs).into(),
        )?;
        changed.push("ожидание команды");
    }
    let hotkey = form.emergency_stop.trim();
    if hotkey != config.hotkeys.emergency_stop.trim() {
        set(&mut doc, "hotkeys", "emergency_stop", hotkey.into())?;
        changed.push("горячая клавиша");
    }
    if form.dry_run != config.general.dry_run {
        set(&mut doc, "general", "dry_run", form.dry_run.into())?;
        changed.push("режим отладки");
    }
    if form.greet != config.general.greet_on_start {
        set(&mut doc, "general", "greet_on_start", form.greet.into())?;
        changed.push("приветствие");
    }
    if form.voice_check != config.speaker.verify {
        set(&mut doc, "speaker", "verify", form.voice_check.into())?;
        changed.push("проверка голоса");
    }
    if form.threshold != threshold_text(&config.speaker.threshold) {
        let value: toml_edit::Value = match form.threshold.parse::<f64>() {
            Ok(number) => number.into(),
            Err(_) => "auto".into(),
        };
        set(&mut doc, "speaker", "threshold", value)?;
        changed.push("строгость проверки голоса");
    }
    Ok((doc.to_string(), changed))
}

/// Записать значение; комментарий справа от старого значения сохраняется.
fn set(
    doc: &mut DocumentMut,
    section: &'static str,
    key: &str,
    value: toml_edit::Value,
) -> Result<(), SettingsError> {
    let table = doc
        .as_table_mut()
        .entry(section)
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or(SettingsError::NotTable(section))?;
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        }
        _ => {
            table.insert(key, Item::Value(value));
        }
    }
    Ok(())
}

/// Порог так, как его показывает окно: «auto» или «0.35».
fn threshold_text(threshold: &SpeakerThreshold) -> String {
    match threshold {
        SpeakerThreshold::Auto => "auto".to_owned(),
        SpeakerThreshold::Fixed(value) => format!("{value:.2}"),
    }
}

fn unknown(field: &'static str, value: &str) -> SettingsError {
    SettingsError::Unknown {
        field,
        value: value.to_owned(),
    }
}

fn volume_percent(volume: f32) -> u32 {
    (volume * 100.0).round().clamp(0.0, 200.0) as u32
}

fn find_device<'a>(names: &'a [String], part: &str) -> Option<&'a str> {
    let part = part.to_lowercase();
    names
        .iter()
        .find(|n| n.to_lowercase().contains(&part))
        .map(String::as_str)
}

fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(user: &str) -> Config {
        let default_text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml"),
        )
        .unwrap();
        Config::from_sources(
            (Path::new("default.toml"), &default_text),
            Some((Path::new("user.toml"), user)),
        )
        .unwrap()
    }

    fn form(config: &Config) -> SettingsForm {
        SettingsForm {
            mic: config
                .audio
                .input_device_name()
                .unwrap_or(SYSTEM_DEFAULT_DEVICE)
                .to_owned(),
            pack: config.voice.pack.clone(),
            tts: config.voice.tts.clone(),
            volume: volume_percent(config.voice.volume),
            llm: config.llm.enabled,
            show_window: config.llm.show_window,
            follow_up_secs: config.session.follow_up_secs,
            emergency_stop: config.hotkeys.emergency_stop.clone(),
            dry_run: config.general.dry_run,
            greet: config.general.greet_on_start,
            voice_check: config.speaker.verify,
            threshold: threshold_text(&config.speaker.threshold),
        }
    }

    #[test]
    fn new_settings_are_written_and_load() {
        let cfg = config(USER);
        let mut f = form(&cfg);
        f.follow_up_secs = 20;
        f.emergency_stop = "Ctrl+Alt+F11".into();
        f.dry_run = true;
        f.threshold = "0.40".into();
        let (text, changed) = apply(USER, &cfg, &f).unwrap();
        assert_eq!(
            changed,
            [
                "ожидание команды",
                "горячая клавиша",
                "режим отладки",
                "строгость проверки голоса"
            ]
        );
        let new = config(&text);
        assert_eq!(new.session.follow_up_secs, 20);
        assert_eq!(new.hotkeys.emergency_stop, "Ctrl+Alt+F11");
        assert!(new.general.dry_run);
        assert_eq!(threshold_text(&new.speaker.threshold), "0.40");
        // Обратно на «auto».
        f.threshold = "auto".into();
        let (text, _) = apply(&text, &new, &f).unwrap();
        assert_eq!(threshold_text(&config(&text).speaker.threshold), "auto");
    }

    const USER: &str = "# Ваши настройки\n[voice]\npack = \"jarvis\"   # голос из фильма\n\n[llm]\nenabled = true    # мой ключ\n";

    #[test]
    fn nothing_changed_writes_nothing() {
        let cfg = config(USER);
        let (text, changed) = apply(USER, &cfg, &form(&cfg)).unwrap();
        assert!(changed.is_empty());
        assert_eq!(text, USER);
    }

    #[test]
    fn changes_keep_comments_and_add_missing_keys() {
        let cfg = config(USER);
        let mut f = form(&cfg);
        f.pack = "default".into();
        f.volume = 80;
        f.mic = "Микрофон (ME6S)".into();
        f.llm = false;
        let (text, changed) = apply(USER, &cfg, &f).unwrap();
        assert_eq!(
            changed,
            ["микрофон", "звуковой пакет", "громкость", "нейросеть"]
        );
        assert!(text.starts_with("# Ваши настройки\n"), "{text}");
        assert!(
            text.contains("pack = \"default\"   # голос из фильма"),
            "{text}"
        );
        assert!(text.contains("enabled = false    # мой ключ"), "{text}");
        assert!(text.contains("volume = 0.8"), "{text}");
        assert!(
            text.contains("[audio]\ninput_device = \"Микрофон (ME6S)\""),
            "{text}"
        );
        // Результат — корректный конфиг с новыми значениями.
        let new = config(&text);
        assert_eq!(new.voice.pack, "default");
        assert!((new.voice.volume - 0.8).abs() < 1e-6);
        assert!(!new.llm.enabled);
    }

    #[test]
    fn empty_user_file_gets_sections() {
        let cfg = config("");
        let mut f = form(&cfg);
        f.show_window = !cfg.llm.show_window;
        let (text, changed) = apply("", &cfg, &f).unwrap();
        assert_eq!(changed, ["окно ответа"]);
        assert_eq!(config(&text).llm.show_window, f.show_window);
    }

    #[test]
    fn broken_user_file_is_not_overwritten() {
        let cfg = config("");
        let err = apply("[voice\npack=", &cfg, &form(&cfg)).unwrap_err();
        assert!(matches!(err, SettingsError::Parse(_)));
        let err = apply(
            "voice = 5\n",
            &cfg,
            &SettingsForm {
                pack: "x".into(),
                ..form(&cfg)
            },
        )
        .unwrap_err();
        assert!(matches!(err, SettingsError::NotTable("voice")));
    }

    #[test]
    fn volume_percent_rounds_and_clamps() {
        assert_eq!(volume_percent(1.0), 100);
        assert_eq!(volume_percent(0.805), 81);
        assert_eq!(volume_percent(5.0), 200);
    }
}
