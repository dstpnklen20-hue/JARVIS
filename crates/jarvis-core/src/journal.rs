//! Журнал действий (раздел 2.5 ТЗ): что услышано, какая команда, с какой
//! уверенностью, было ли разрешение и как получено, чем кончилось.
//!
//! Отдельно от технического лога: `logs/actions.ГГГГ-ММ-ДД.jsonl`, одна
//! запись — одна строка JSON, ротация раз в сутки (`logging.max_files`).

use std::io::Write as _;
use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// Как получено разрешение (или почему его не было).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionNote {
    /// Разрешено «всегда» раньше.
    Saved,
    /// Спросили — разрешили один раз.
    Once,
    /// Спросили — разрешили навсегда.
    Always,
    /// Спросили — отказали.
    No,
    /// Спросили — запретили навсегда.
    Never,
    /// Запрещено «никогда» раньше.
    SavedNever,
    /// Спросили — ответа не было 10 секунд.
    Timeout,
    /// Встроенная команда Джарвиса (стоп и т. п.) — разрешение не нужно.
    Builtin,
    /// Режим dry-run: не выполнялось.
    DryRun,
}

/// Откуда пришёл ответ на вопрос-разрешение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    Voice,
    Button,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub time: String,
    /// Распознанный текст.
    pub heard: String,
    pub command: String,
    /// Значения слотов («громкость на 40» → level = 40).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub slots: crate::slots::SlotValues,
    /// Уверенность сопоставления (0–1).
    pub score: f32,
    pub permission: PermissionNote,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<AnswerSource>,
    /// `ok`, `failed`, `not_executed`.
    pub result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct ActionJournal {
    file: Mutex<RollingFileAppender>,
}

impl ActionJournal {
    pub fn open(dir: &Path, max_files: usize) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let file = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("actions")
            .filename_suffix("jsonl")
            .max_log_files(max_files)
            .build(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Дописывает запись. Ошибка записи — в технический лог, работа продолжается.
    pub fn write(&self, entry: &Entry) {
        let line = match serde_json::to_string(entry) {
            Ok(line) => line,
            Err(e) => {
                tracing::error!(error = %e, "журнал действий: запись не сериализуется");
                return;
            }
        };
        let Ok(mut file) = self.file.lock() else {
            tracing::error!("журнал действий недоступен");
            return;
        };
        if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
            tracing::error!(error = %e, "не удалось записать журнал действий");
        }
    }
}

/// Текущее время для записи журнала (местное, с часовым поясом).
/// Местный час (0–23) — для приветствия по времени суток.
pub fn local_hour() -> u8 {
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .hour()
}

pub fn now() -> String {
    let format = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]"
    );
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(&format)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_json_line_per_entry() {
        let dir = tempfile::tempdir().unwrap();
        let journal = ActionJournal::open(dir.path(), 3).unwrap();
        let entry = Entry {
            time: "2026-10-12T02:00:00+04:00".into(),
            heard: "открой ютуб".into(),
            command: "open_youtube".into(),
            slots: Default::default(),
            score: 1.0,
            permission: PermissionNote::Once,
            answered_by: Some(AnswerSource::Voice),
            result: "ok",
            error: None,
        };
        journal.write(&entry);
        journal.write(&Entry {
            result: "failed",
            error: Some("нет браузера".into()),
            answered_by: None,
            permission: PermissionNote::Saved,
            ..entry.clone()
        });

        let file = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("actions.")
        );
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["command"], "open_youtube");
        assert_eq!(first["permission"], "once");
        assert_eq!(first["answered_by"], "voice");
        assert!(first.get("error").is_none());
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["result"], "failed");
        assert_eq!(second["permission"], "saved");
        assert_eq!(second["error"], "нет браузера");
    }

    #[test]
    fn time_has_offset() {
        let t = now();
        assert!(t.len() >= 25 && t.contains('T'), "{t}");
    }
}
