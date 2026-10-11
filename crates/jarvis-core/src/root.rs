//! Поиск корня проекта — папки, в которой лежит `config/default.toml`.
//!
//! Все относительные пути конфига (модели, логи, команды) считаются от корня,
//! поэтому `jarvis` работает одинаково из `cargo run`, из `target/release`
//! и из установленной папки.

use std::path::{Path, PathBuf};

use crate::config::DEFAULT_FILE;

#[derive(Debug, thiserror::Error)]
#[error(
    "не найден корень проекта: ни в одной из папок нет {DEFAULT_FILE}\nпроверены: {}",
    .searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
)]
pub struct RootNotFound {
    pub searched: Vec<PathBuf>,
}

/// Ищет корень проекта.
///
/// Если `explicit` задан, проверяется только он. Иначе — текущая папка и её
/// родители, затем папка исполняемого файла и её родители.
pub fn find_root(explicit: Option<&Path>) -> Result<PathBuf, RootNotFound> {
    let mut searched = Vec::new();

    if let Some(dir) = explicit {
        if is_root(dir) {
            return Ok(dir.to_path_buf());
        }
        searched.push(dir.to_path_buf());
        return Err(RootNotFound { searched });
    }

    let starts = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
    ];
    for start in starts.into_iter().flatten() {
        if let Some(found) = search_upwards(&start) {
            return Ok(found);
        }
        searched.push(start);
    }
    Err(RootNotFound { searched })
}

/// Путь внутри проекта из константы вида `"config/default.toml"`.
///
/// `Path::join` со строкой через `/` дал бы на Windows смешанные
/// разделители (`F:\JARVIS\config/default.toml`) — собираем по частям.
pub fn project_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn search_upwards(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| is_root(dir))
        .map(Path::to_path_buf)
}

fn is_root(dir: &Path) -> bool {
    project_path(dir, DEFAULT_FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(project_path(dir.path(), DEFAULT_FILE), "").unwrap();
        dir
    }

    #[test]
    fn finds_root_from_nested_dir() {
        let root = make_root();
        let nested = root.path().join("target").join("debug");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(search_upwards(&nested).unwrap(), root.path());
    }

    #[test]
    fn project_path_uses_native_separators() {
        let path = project_path(Path::new("root"), "config/default.toml");
        assert_eq!(path, Path::new("root").join("config").join("default.toml"));
    }

    #[test]
    fn explicit_root_is_checked_exactly() {
        let root = make_root();
        assert_eq!(find_root(Some(root.path())).unwrap(), root.path());

        let empty = tempfile::tempdir().unwrap();
        let err = find_root(Some(empty.path())).unwrap_err();
        assert_eq!(err.searched, vec![empty.path().to_path_buf()]);
    }
}
