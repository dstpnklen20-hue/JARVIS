//! Ядро Джарвиса.
//!
//! Этап 0: поиск корня проекта, загрузка и валидация конфига, логирование,
//! описание скачиваемых ресурсов (модели, нативные библиотеки).

pub mod assets;
pub mod config;
pub mod logging;
pub mod root;

pub use config::Config;

/// Ошибка вместе со всеми причинами: «верхний уровень: причина: причина…».
///
/// Типы ошибок проекта не повторяют причину в своём тексте, а отдают её
/// через `source()`, поэтому для показа человеку нужна вся цепочка.
pub fn error_chain(err: &dyn std::error::Error) -> String {
    // Некоторые ошибки (например, toml) заканчиваются переводом строки.
    let mut text = err.to_string().trim_end().to_owned();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(cause.to_string().trim_end());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("верх")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn error_chain_joins_all_causes() {
        let err = Outer(std::io::Error::other("низ"));
        assert_eq!(error_chain(&err), "верх: низ");
    }
}
