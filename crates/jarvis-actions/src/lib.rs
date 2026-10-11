//! Исполнители действий команд (раздел 6 ТЗ).
//!
//! Системные вызовы спрятаны за трейтом [`System`]: в работе — реализация
//! из `jarvis-platform`, в тестах — подделка, записывающая вызовы.
//! Все проверки параметров уже сделаны при загрузке манифеста
//! (`jarvis_core::commands`); здесь — только исполнение.

use std::path::PathBuf;

use jarvis_core::commands::{ActionSpec, MediaOp, validate_url};

/// Громкость по умолчанию меняется на 10 % (одно нажатие ≈ 2 %).
pub const DEFAULT_VOLUME_STEPS: u32 = 5;

/// Системные возможности, нужные исполнителям.
pub trait System: Send + Sync {
    fn open_url(&self, url: &str) -> Result<(), String>;
    fn media_key(&self, op: MediaOp, presses: u32) -> Result<(), String>;
}

/// Чем закончилось действие.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// `say`: что проиграть (проигрывает ядро — у него звук).
    Play(Sound),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sound {
    /// Ответ из звукового пакета по ключу.
    Reply(String),
    /// WAV из папки пакета команд (путь уже проверен и абсолютный).
    File(PathBuf),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionError {
    #[error("не удалось открыть адрес: {0}")]
    OpenUrl(String),
    #[error("не удалось нажать мультимедиа-клавишу: {0}")]
    Media(String),
    #[error("недопустимые параметры действия: {0}")]
    Invalid(String),
}

/// Выполняет действие. `command_dir` — папка пакета команды.
pub fn execute(
    action: &ActionSpec,
    command_dir: &std::path::Path,
    system: &dyn System,
) -> Result<Outcome, ActionError> {
    match action {
        ActionSpec::Say {
            reply: Some(key), ..
        } => Ok(Outcome::Play(Sound::Reply(key.clone()))),
        ActionSpec::Say {
            file: Some(file), ..
        } => Ok(Outcome::Play(Sound::File(command_dir.join(file)))),
        ActionSpec::Say { .. } => Err(ActionError::Invalid("say без reply и file".into())),
        ActionSpec::OpenUrl { url } => {
            // Повторная проверка — дёшево, а адрес уходит в систему.
            validate_url(url).map_err(ActionError::Invalid)?;
            system.open_url(url).map_err(ActionError::OpenUrl)?;
            Ok(Outcome::Done)
        }
        ActionSpec::Media { operation, steps } => {
            let presses = if operation.is_volume() {
                steps.unwrap_or(DEFAULT_VOLUME_STEPS)
            } else {
                1
            };
            system
                .media_key(*operation, presses)
                .map_err(ActionError::Media)?;
            Ok(Outcome::Done)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    impl System for Fake {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("open {url}"));
            if self.fail {
                Err("нет браузера".into())
            } else {
                Ok(())
            }
        }
        fn media_key(&self, op: MediaOp, presses: u32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("{op:?}×{presses}"));
            if self.fail {
                Err("SendInput".into())
            } else {
                Ok(())
            }
        }
    }

    fn calls(f: &Fake) -> Vec<String> {
        f.calls.lock().unwrap().clone()
    }

    #[test]
    fn open_url_calls_system() {
        let fake = Fake::default();
        let action = ActionSpec::OpenUrl {
            url: "https://www.youtube.com".into(),
        };
        assert_eq!(execute(&action, Path::new("."), &fake), Ok(Outcome::Done));
        assert_eq!(calls(&fake), ["open https://www.youtube.com"]);
    }

    #[test]
    fn dangerous_url_never_reaches_system() {
        let fake = Fake::default();
        let action = ActionSpec::OpenUrl {
            url: "file:///C:/Windows/System32/cmd.exe".into(),
        };
        assert!(matches!(
            execute(&action, Path::new("."), &fake),
            Err(ActionError::Invalid(_))
        ));
        assert!(calls(&fake).is_empty());
    }

    #[test]
    fn media_presses_once_volume_uses_steps() {
        let fake = Fake::default();
        let next = ActionSpec::Media {
            operation: MediaOp::Next,
            steps: None,
        };
        let up = ActionSpec::Media {
            operation: MediaOp::VolumeUp,
            steps: Some(3),
        };
        let down = ActionSpec::Media {
            operation: MediaOp::VolumeDown,
            steps: None,
        };
        for a in [&next, &up, &down] {
            execute(a, Path::new("."), &fake).unwrap();
        }
        assert_eq!(calls(&fake), ["Next×1", "VolumeUp×3", "VolumeDown×5"]);
    }

    #[test]
    fn say_returns_sound_for_core() {
        let fake = Fake::default();
        let reply = ActionSpec::Say {
            reply: Some("ok".into()),
            file: None,
        };
        assert_eq!(
            execute(&reply, Path::new("."), &fake),
            Ok(Outcome::Play(Sound::Reply("ok".into())))
        );
        let file = ActionSpec::Say {
            reply: None,
            file: Some("hello.wav".into()),
        };
        assert_eq!(
            execute(&file, Path::new("commands/x"), &fake),
            Ok(Outcome::Play(Sound::File(
                Path::new("commands/x").join("hello.wav")
            )))
        );
        assert!(calls(&fake).is_empty());
    }

    #[test]
    fn system_failures_become_errors() {
        let fake = Fake {
            fail: true,
            ..Fake::default()
        };
        let action = ActionSpec::OpenUrl {
            url: "https://a.com".into(),
        };
        assert_eq!(
            execute(&action, Path::new("."), &fake),
            Err(ActionError::OpenUrl("нет браузера".into()))
        );
    }
}
