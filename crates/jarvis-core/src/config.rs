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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub audio: Audio,
    pub session: Session,
    pub logging: Logging,
    pub paths: Paths,
}

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
}
