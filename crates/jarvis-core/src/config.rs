//! Конфигурация: `config/default.toml` + переопределения из `config/user.toml`.
//!
//! Порядок загрузки:
//! 1. `default.toml` целиком разбирается в [`Config`] — так ошибки в нём
//!    показываются со строкой и столбцом.
//! 2. `user.toml` (если есть) проверяется на неизвестные ключи и
//!    накладывается поверх значений по умолчанию.
//! 3. Итог разбирается в [`Config`] и проходит [`Config::validate`].
//!
//! С битым конфигом приложение не стартует: ошибка называет файл, параметр
//! и причину.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::{Table, Value};

use crate::root::project_path;

pub const DEFAULT_FILE: &str = "config/default.toml";
pub const USER_FILE: &str = "config/user.toml";

/// Значение `audio.input_device`, означающее системный микрофон.
pub const SYSTEM_DEFAULT_DEVICE: &str = "default";

pub const FOLLOW_UP_SECS_RANGE: std::ops::RangeInclusive<u32> = 3..=60;
pub const LOG_MAX_FILES_RANGE: std::ops::RangeInclusive<usize> = 1..=365;
pub const LISTEN_SECS_RANGE: std::ops::RangeInclusive<u32> = 2..=30;
pub const MAX_EDIT_DISTANCE: usize = 2;
pub const VOLUME_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
pub const END_SILENCE_MS_RANGE: std::ops::RangeInclusive<u32> = 300..=2000;
pub const HOTWORDS_SCORE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=4.0;
pub const EARLY_STABLE_MS_RANGE: std::ops::RangeInclusive<u32> = 0..=2000;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub audio: Audio,
    pub wake: Wake,
    pub listen: Listen,
    pub nlu: Nlu,
    pub speaker: Speaker,
    pub vad: Vad,
    pub voice: Voice,
    pub hotkeys: Hotkeys,
    pub session: Session,
    pub logging: Logging,
    pub paths: Paths,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wake {
    /// Слова активации в том виде, как их пишет распознаватель.
    pub words: Vec<String>,
    /// Допустимое расстояние Левенштейна до слова активации.
    pub max_edit_distance: usize,
    /// Сколько секунд после «Джарвис» ждать команду.
    pub listen_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hotkeys {
    /// «Ctrl+Alt+F12»; пусто — без горячей клавиши. Синтаксис проверяет
    /// платформа при регистрации (там же список клавиш).
    pub emergency_stop: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listen {
    /// Тишина в конце фразы, мс.
    pub end_silence_ms: u32,
    /// Сила подсказок распознавателю (0 — выключены).
    pub hotwords_score: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nlu {
    pub min_score: f32,
    pub ambiguity_margin: f32,
    pub early_score: f32,
    pub early_stable_ms: u32,
    /// Решение по однозначному началу фразы: доля длины фразы (0 — выключено).
    pub prefix_min_ratio: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Speaker {
    pub verify: bool,
    pub threshold: SpeakerThreshold,
}

/// Порог сходства голоса: подобранный при `--enroll` или заданный вручную.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpeakerThreshold {
    Auto,
    Fixed(f32),
}

impl<'de> Deserialize<'de> for SpeakerThreshold {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(f64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Number(n) => Ok(Self::Fixed(n as f32)),
            Raw::Text(s) if s == "auto" => Ok(Self::Auto),
            Raw::Text(s) => Err(serde::de::Error::custom(format!(
                "ожидалось \"auto\" или число 0–1, получено \"{s}\""
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VadEngine {
    Silero,
    Ten,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vad {
    pub engine: VadEngine,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    /// Папка звукового пакета внутри `paths.sounds`.
    pub pack: String,
    pub volume: f32,
    /// Голос синтеза («dmitri»…) или «none».
    pub tts: String,
    pub tts_speed: f32,
}

impl Voice {
    /// Голос синтеза, если он включён.
    pub fn tts_voice(&self) -> Option<&str> {
        let name = self.tts.trim();
        (!name.eq_ignore_ascii_case("none") && !name.is_empty()).then_some(name)
    }
}

pub const TTS_SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    /// Распознавать и журналировать команды, но не выполнять их.
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    /// `"default"` или часть имени устройства.
    pub input_device: String,
}

impl Audio {
    /// Имя (подстрока) выбранного микрофона или `None` для системного.
    pub fn input_device_name(&self) -> Option<&str> {
        let name = self.input_device.trim();
        if name.eq_ignore_ascii_case(SYSTEM_DEFAULT_DEVICE) {
            None
        } else {
            Some(name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub follow_up_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    pub level: String,
    pub dir: PathBuf,
    pub max_files: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    pub models: PathBuf,
    pub native_libs: PathBuf,
    pub commands: PathBuf,
    pub sounds: PathBuf,
    pub data: PathBuf,
}

/// Одна найденная проблема в значениях конфига.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub key: &'static str,
    pub reason: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}`: {}", self.key, self.reason)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать {}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ошибка в {}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{}: неизвестный параметр `{key}` (такого нет в {DEFAULT_FILE} — опечатка?)", path.display())]
    UnknownKey { path: PathBuf, key: String },
    #[error("{}: `{key}` должен быть секцией [{key}], а не значением", path.display())]
    NotATable { path: PathBuf, key: String },
    #[error("неверные значения в конфиге:\n{}", list(.0))]
    Invalid(Vec<Problem>),
}

fn list(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Config {
    /// Загружает конфиг из корня проекта.
    pub fn load(root: &Path) -> Result<Self, ConfigError> {
        let default_path = project_path(root, DEFAULT_FILE);
        let default_text = read(&default_path)?;
        let user_path = project_path(root, USER_FILE);
        let user_text = match std::fs::read_to_string(&user_path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(ConfigError::Read {
                    path: user_path,
                    source,
                });
            }
        };
        Self::from_sources(
            (&default_path, &default_text),
            user_text.as_deref().map(|text| (user_path.as_path(), text)),
        )
    }

    /// Собирает конфиг из текстов файлов. Пути нужны только для сообщений.
    pub fn from_sources(
        (default_path, default_text): (&Path, &str),
        user: Option<(&Path, &str)>,
    ) -> Result<Self, ConfigError> {
        let parse_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| ConfigError::Parse { path, source }
        };

        // Сначала строгий разбор default.toml: ошибки со строкой и столбцом.
        let defaults: Config = toml::from_str(default_text).map_err(parse_err(default_path))?;

        let config = match user {
            None => defaults,
            Some((user_path, user_text)) => {
                let mut merged: Table =
                    toml::from_str(default_text).map_err(parse_err(default_path))?;
                let overrides: Table = toml::from_str(user_text).map_err(parse_err(user_path))?;
                merge(&mut merged, overrides, "", user_path)?;
                Config::deserialize(merged).map_err(parse_err(user_path))?
            }
        };

        config.validate().map_err(ConfigError::Invalid)?;
        Ok(config)
    }

    /// Проверяет значения, которые не выразить типами.
    pub fn validate(&self) -> Result<(), Vec<Problem>> {
        let mut problems = Vec::new();
        let mut check = |ok: bool, key: &'static str, reason: String| {
            if !ok {
                problems.push(Problem { key, reason });
            }
        };

        check(
            !self.audio.input_device.trim().is_empty(),
            "audio.input_device",
            format!("пустое имя; для системного микрофона укажите \"{SYSTEM_DEFAULT_DEVICE}\""),
        );
        check(
            !self.wake.words.is_empty(),
            "wake.words",
            "нужно хотя бы одно слово активации".into(),
        );
        check(
            self.wake
                .words
                .iter()
                .all(|w| w.trim().chars().count() >= 3),
            "wake.words",
            "слово активации должно быть не короче 3 букв".into(),
        );
        check(
            self.wake.max_edit_distance <= MAX_EDIT_DISTANCE,
            "wake.max_edit_distance",
            format!(
                "{} больше допустимого {MAX_EDIT_DISTANCE}",
                self.wake.max_edit_distance
            ),
        );
        check(
            LISTEN_SECS_RANGE.contains(&self.wake.listen_secs),
            "wake.listen_secs",
            out_of_range(self.wake.listen_secs, &LISTEN_SECS_RANGE),
        );
        check(
            END_SILENCE_MS_RANGE.contains(&self.listen.end_silence_ms),
            "listen.end_silence_ms",
            out_of_range(self.listen.end_silence_ms, &END_SILENCE_MS_RANGE),
        );
        check(
            HOTWORDS_SCORE_RANGE.contains(&self.listen.hotwords_score),
            "listen.hotwords_score",
            out_of_range(self.listen.hotwords_score, &HOTWORDS_SCORE_RANGE),
        );
        for (key, value) in [
            ("nlu.min_score", self.nlu.min_score),
            ("nlu.ambiguity_margin", self.nlu.ambiguity_margin),
            ("nlu.early_score", self.nlu.early_score),
            ("nlu.prefix_min_ratio", self.nlu.prefix_min_ratio),
        ] {
            check(
                (0.0..=1.0).contains(&value),
                key,
                out_of_range(value, &(0.0..=1.0)),
            );
        }
        check(
            self.nlu.early_score >= self.nlu.min_score,
            "nlu.early_score",
            format!(
                "{} меньше nlu.min_score ({}) — раннее срабатывание было бы менее строгим, чем обычное",
                self.nlu.early_score, self.nlu.min_score
            ),
        );
        check(
            EARLY_STABLE_MS_RANGE.contains(&self.nlu.early_stable_ms),
            "nlu.early_stable_ms",
            out_of_range(self.nlu.early_stable_ms, &EARLY_STABLE_MS_RANGE),
        );
        if let SpeakerThreshold::Fixed(t) = self.speaker.threshold {
            check(
                (0.0..=1.0).contains(&t),
                "speaker.threshold",
                format!("{t} вне диапазона 0–1"),
            );
        }
        check(
            !self.voice.pack.trim().is_empty(),
            "voice.pack",
            "имя звукового пакета не может быть пустым".into(),
        );
        check(
            VOLUME_RANGE.contains(&self.voice.volume),
            "voice.volume",
            out_of_range(self.voice.volume, &VOLUME_RANGE),
        );
        check(
            self.voice
                .tts
                .trim()
                .chars()
                .all(|c| c.is_ascii_lowercase()),
            "voice.tts",
            format!(
                "«{}» — имя голоса латиницей (dmitri, irina…) или none",
                self.voice.tts
            ),
        );
        check(
            TTS_SPEED_RANGE.contains(&self.voice.tts_speed),
            "voice.tts_speed",
            out_of_range(self.voice.tts_speed, &TTS_SPEED_RANGE),
        );
        check(
            FOLLOW_UP_SECS_RANGE.contains(&self.session.follow_up_secs),
            "session.follow_up_secs",
            format!(
                "{} вне допустимого диапазона {}–{}",
                self.session.follow_up_secs,
                FOLLOW_UP_SECS_RANGE.start(),
                FOLLOW_UP_SECS_RANGE.end()
            ),
        );
        if let Err(e) = crate::logging::parse_filter(&self.logging.level) {
            check(false, "logging.level", crate::error_chain(&e));
        }
        check(
            LOG_MAX_FILES_RANGE.contains(&self.logging.max_files),
            "logging.max_files",
            format!(
                "{} вне допустимого диапазона {}–{}",
                self.logging.max_files,
                LOG_MAX_FILES_RANGE.start(),
                LOG_MAX_FILES_RANGE.end()
            ),
        );
        for (key, path) in [
            ("logging.dir", &self.logging.dir),
            ("paths.models", &self.paths.models),
            ("paths.native_libs", &self.paths.native_libs),
            ("paths.commands", &self.paths.commands),
            ("paths.sounds", &self.paths.sounds),
            ("paths.data", &self.paths.data),
        ] {
            check(
                !path.as_os_str().is_empty(),
                key,
                "путь не может быть пустым".into(),
            );
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

fn out_of_range<T: fmt::Display>(value: T, range: &std::ops::RangeInclusive<T>) -> String {
    format!(
        "{value} вне допустимого диапазона {}–{}",
        range.start(),
        range.end()
    )
}

/// Абсолютный путь для значения из конфига: относительные — от корня проекта.
pub fn resolve(root: &Path, path: &Path) -> PathBuf {
    root.join(path)
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })
}

/// Накладывает `overrides` на `base`. Ключ, которого нет в `base`, — ошибка:
/// в default.toml описаны все параметры, значит это опечатка.
fn merge(base: &mut Table, overrides: Table, prefix: &str, path: &Path) -> Result<(), ConfigError> {
    for (key, value) in overrides {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (base.get_mut(&key), value) {
            (None, _) => {
                return Err(ConfigError::UnknownKey {
                    path: path.to_path_buf(),
                    key: full_key,
                });
            }
            (Some(Value::Table(base_table)), Value::Table(table)) => {
                merge(base_table, table, &full_key, path)?;
            }
            (Some(Value::Table(_)), _) => {
                return Err(ConfigError::NotATable {
                    path: path.to_path_buf(),
                    key: full_key,
                });
            }
            // Несовпадение типа скаляра поймает разбор в Config.
            (Some(slot), value) => *slot = value,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = include_str!("../../../config/default.toml");

    fn load(user: Option<&str>) -> Result<Config, ConfigError> {
        Config::from_sources(
            (Path::new(DEFAULT_FILE), DEFAULT),
            user.map(|text| (Path::new(USER_FILE), text)),
        )
    }

    #[test]
    fn shipped_default_is_valid() {
        let config = load(None).unwrap();
        assert_eq!(config.audio.input_device_name(), None);
        assert!(!config.general.dry_run);
        assert!(FOLLOW_UP_SECS_RANGE.contains(&config.session.follow_up_secs));
    }

    #[test]
    fn user_overrides_only_given_keys() {
        let config = load(Some(
            "[audio]\ninput_device = \"Fuxi-H3\"\n[general]\ndry_run = true\n",
        ))
        .unwrap();
        let defaults = load(None).unwrap();
        assert_eq!(config.audio.input_device_name(), Some("Fuxi-H3"));
        assert!(config.general.dry_run);
        assert_eq!(config.session, defaults.session);
        assert_eq!(config.logging, defaults.logging);
    }

    #[test]
    fn empty_user_file_is_fine() {
        assert_eq!(load(Some("")).unwrap(), load(None).unwrap());
    }

    #[test]
    fn unknown_key_in_user_is_reported_with_path() {
        let err = load(Some("[audio]\ninput_devise = \"x\"\n")).unwrap_err();
        match &err {
            ConfigError::UnknownKey { path, key } => {
                assert_eq!(path, Path::new(USER_FILE));
                assert_eq!(key, "audio.input_devise");
            }
            other => panic!("ожидали UnknownKey, получили {other:?}"),
        }
        assert!(crate::error_chain(&err).contains("audio.input_devise"));
    }

    #[test]
    fn unknown_section_in_user_is_reported() {
        let err = load(Some("[audoi]\ninput_device = \"x\"\n")).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownKey { ref key, .. } if key == "audoi"));
    }

    #[test]
    fn scalar_instead_of_section_is_reported() {
        let err = load(Some("audio = 5\n")).unwrap_err();
        assert!(matches!(err, ConfigError::NotATable { ref key, .. } if key == "audio"));
    }

    #[test]
    fn wrong_type_in_user_names_key() {
        let err = load(Some("[session]\nfollow_up_secs = \"двенадцать\"\n")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { ref path, .. } if path == Path::new(USER_FILE)));
        let text = crate::error_chain(&err);
        assert!(text.contains("follow_up_secs"), "нет имени ключа: {text}");
    }

    #[test]
    fn syntax_error_reports_line() {
        let err = load(Some("[audio\ninput_device = 1\n")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(crate::error_chain(&err).contains("line 1"), "{err}");
    }

    #[test]
    fn unknown_key_in_default_is_rejected() {
        let broken = format!("{DEFAULT}\n[extra]\nx = 1\n");
        let err = Config::from_sources((Path::new(DEFAULT_FILE), &broken), None).unwrap_err();
        assert!(crate::error_chain(&err).contains("extra"), "{err}");
    }

    #[test]
    fn missing_key_in_default_is_rejected() {
        let broken = DEFAULT.replace("follow_up_secs = 12", "");
        let err = Config::from_sources((Path::new(DEFAULT_FILE), &broken), None).unwrap_err();
        assert!(crate::error_chain(&err).contains("follow_up_secs"), "{err}");
    }

    #[test]
    fn out_of_range_values_are_all_reported() {
        let err = load(Some(
            "[session]\nfollow_up_secs = 100\n[logging]\nmax_files = 0\nlevel = \"loud\"\n[audio]\ninput_device = \"  \"\n",
        ))
        .unwrap_err();
        let ConfigError::Invalid(problems) = err else {
            panic!("ожидали Invalid, получили {err:?}");
        };
        let keys: Vec<_> = problems.iter().map(|p| p.key).collect();
        assert_eq!(
            keys,
            [
                "audio.input_device",
                "session.follow_up_secs",
                "logging.level",
                "logging.max_files"
            ]
        );
    }

    #[test]
    fn negative_number_is_rejected() {
        assert!(load(Some("[session]\nfollow_up_secs = -5\n")).is_err());
    }

    #[test]
    fn empty_path_is_rejected() {
        let err = load(Some("[paths]\nmodels = \"\"\n")).unwrap_err();
        assert!(crate::error_chain(&err).contains("paths.models"), "{err}");
    }

    #[test]
    fn default_device_keyword_is_case_insensitive() {
        let config = load(Some("[audio]\ninput_device = \" Default \"\n")).unwrap();
        assert_eq!(config.audio.input_device_name(), None);
    }

    #[test]
    fn speaker_threshold_accepts_auto_and_numbers() {
        assert_eq!(
            load(None).unwrap().speaker.threshold,
            SpeakerThreshold::Auto
        );
        let config = load(Some("[speaker]\nthreshold = 0.55\n")).unwrap();
        assert_eq!(config.speaker.threshold, SpeakerThreshold::Fixed(0.55));
        // Целое число в TOML — тоже число.
        let config = load(Some("[speaker]\nthreshold = 1\n")).unwrap();
        assert_eq!(config.speaker.threshold, SpeakerThreshold::Fixed(1.0));
    }

    #[test]
    fn speaker_threshold_rejects_bad_values() {
        let err = load(Some("[speaker]\nthreshold = \"авто\"\n")).unwrap_err();
        assert!(crate::error_chain(&err).contains("auto"), "{err}");
        let err = load(Some("[speaker]\nthreshold = 1.5\n")).unwrap_err();
        assert!(
            crate::error_chain(&err).contains("speaker.threshold"),
            "{err}"
        );
    }

    #[test]
    fn wake_and_voice_values_are_validated() {
        let err = load(Some(
            "[wake]\nwords = []\nmax_edit_distance = 3\nlisten_secs = 1\n[voice]\nvolume = 3.0\npack = \"\"\n",
        ))
        .unwrap_err();
        let ConfigError::Invalid(problems) = err else {
            panic!("ожидали Invalid, получили {err:?}");
        };
        let keys: Vec<_> = problems.iter().map(|p| p.key).collect();
        assert_eq!(
            keys,
            [
                "wake.words",
                "wake.max_edit_distance",
                "wake.listen_secs",
                "voice.pack",
                "voice.volume"
            ]
        );
    }

    #[test]
    fn wake_words_are_replaced_not_merged() {
        let config = load(Some("[wake]\nwords = [\"пятница\"]\n")).unwrap();
        assert_eq!(config.wake.words, ["пятница"]);
    }

    #[test]
    fn listen_and_nlu_values_are_validated() {
        let err = load(Some(
            "[listen]\nend_silence_ms = 100\nhotwords_score = 9\n[nlu]\nmin_score = 0.9\nearly_score = 0.8\nambiguity_margin = 2\n",
        ))
        .unwrap_err();
        let ConfigError::Invalid(problems) = err else {
            panic!("ожидали Invalid, получили {err:?}");
        };
        let keys: Vec<_> = problems.iter().map(|p| p.key).collect();
        assert_eq!(
            keys,
            [
                "listen.end_silence_ms",
                "listen.hotwords_score",
                "nlu.ambiguity_margin",
                "nlu.early_score"
            ]
        );
    }

    #[test]
    fn unknown_vad_engine_is_rejected() {
        let err = load(Some("[vad]\nengine = \"webrtc\"\n")).unwrap_err();
        assert!(crate::error_chain(&err).contains("webrtc"), "{err}");
    }
}
