//! Логирование через `tracing`: в консоль (stderr) и в файл с суточной ротацией.
//!
//! Журнал действий (раздел 2.5 ТЗ) — отдельная подсистема, появится на этапе 3.

use std::path::Path;

use time::UtcOffset;
use time::macros::format_description;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::OffsetTime;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::{self, Logging};

const LEVELS: [&str; 6] = ["trace", "debug", "info", "warn", "error", "off"];

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("неверный фильтр логов `{directive}`: уровень должен быть одним из {}", LEVELS.join(", "))]
    BadLevel { directive: String },
    #[error("неверный фильтр логов")]
    Filter(#[from] tracing_subscriber::filter::ParseError),
    #[error("не удалось создать папку логов {}", dir.display())]
    CreateDir {
        dir: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("не удалось открыть папку логов {}", dir.display())]
    File {
        dir: std::path::PathBuf,
        source: tracing_appender::rolling::InitError,
    },
    #[error("логирование уже инициализировано")]
    AlreadyInit(#[from] tracing_subscriber::util::TryInitError),
}

/// Держите значение живым до выхода из программы: при его удалении
/// дописываются буферизованные строки лога.
#[must_use = "без guard файл лога может потерять последние строки"]
pub struct LogGuard {
    _file: WorkerGuard,
}

/// Проверяет строку фильтра вида `info` или `info,jarvis_core=debug`.
///
/// `EnvFilter` принимает любое слово как имя модуля, поэтому опечатка
/// в уровне (`"inf"`) прошла бы молча. Здесь уровни проверяются явно.
pub fn parse_filter(spec: &str) -> Result<EnvFilter, LoggingError> {
    let mut directives = Vec::new();
    for directive in spec.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        // EnvFilter не принимает пробелы, а людям естественно их ставить.
        let directive: String = directive.split_whitespace().collect();
        let level = directive
            .rsplit_once('=')
            .map_or(directive.as_str(), |(_, l)| l);
        if !LEVELS.contains(&level.to_ascii_lowercase().as_str()) {
            return Err(LoggingError::BadLevel { directive });
        }
        directives.push(directive);
    }
    Ok(EnvFilter::try_new(directives.join(","))?)
}

/// Включает логирование. `RUST_LOG`, если задан, важнее `logging.level`.
///
/// Вызывайте в начале `main`, до запуска потоков: местный часовой пояс
/// определяется один раз здесь (в многопоточной программе `time` может
/// отказаться его определять — тогда время пишется в UTC).
pub fn init(settings: &Logging, root: &Path) -> Result<LogGuard, LoggingError> {
    let filter = match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.trim().is_empty() => EnvFilter::try_new(spec)?,
        _ => parse_filter(&settings.level)?,
    };

    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let timer = OffsetTime::new(
        offset,
        format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]"
        ),
    );

    let dir = config::resolve(root, &settings.dir);
    // Без готовой папки tracing-appender печатает ошибку при очистке старых логов.
    std::fs::create_dir_all(&dir).map_err(|source| LoggingError::CreateDir {
        dir: dir.clone(),
        source,
    })?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("jarvis")
        .filename_suffix("log")
        .max_log_files(settings.max_files)
        .build(&dir)
        .map_err(|source| LoggingError::File { dir, source })?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_timer(timer.clone())
                .with_target(true),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_timer(timer)
                .with_ansi(false)
                .with_target(true)
                .with_thread_names(true),
        )
        .try_init()?;

    Ok(LogGuard { _file: guard })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_levels_and_per_target_levels() {
        for spec in [
            "info",
            "WARN",
            "info,jarvis_core=debug",
            "off",
            " debug , a=trace ",
        ] {
            assert!(parse_filter(spec).is_ok(), "{spec}");
        }
    }

    #[test]
    fn rejects_unknown_levels() {
        for spec in ["loud", "inf", "info,jarvis_core=verbose"] {
            assert!(
                matches!(parse_filter(spec), Err(LoggingError::BadLevel { .. })),
                "{spec}"
            );
        }
    }
}
