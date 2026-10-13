//! Скачиваемые ресурсы: модели и нативные библиотеки.
//!
//! Единый источник правды — `scripts/assets.json`. Его читают и
//! `scripts/download-models.ps1` (скачивание, проверка SHA-256, распаковка),
//! и `jarvis --check` (проверка, что всё на месте).
//!
//! После успешной проверки хеша и распаковки скрипт кладёт в папку ресурса
//! файл-метку [`MARKER_FILE`] с SHA-256 архива. Так `--check` за миллисекунды
//! понимает, что папка получена из проверенного архива нужной версии,
//! не пересчитывая хеши сотен мегабайт.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{self, Config};
use crate::root::project_path;

pub const MANIFEST_FILE: &str = "scripts/assets.json";
pub const MARKER_FILE: &str = ".jarvis-verified";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    /// Короткое имя для отчётов, например «Vosk small-ru 0.22».
    pub name: String,
    /// Зачем нужен ресурс.
    pub description: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Архив распаковывается, файл кладётся как есть.
    pub kind: AssetKind,
    /// В какую папку из `[paths]` конфига распаковывать.
    pub base: AssetBase,
    /// Папка верхнего уровня внутри архива.
    pub dir: String,
    /// Обязателен ли ресурс для работы (необязательные качаются по флагу).
    pub required: bool,
    /// Нужен только для сборки (библиотека, которая вшивается в exe):
    /// в установленной программе его нет — и это не ошибка.
    #[serde(default)]
    pub build_only: bool,
    /// Файлы, наличие которых проверяет `--check` (пути внутри `dir`).
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// Архив (`.zip`, `.tar.bz2`) с папкой `dir` верхнего уровня внутри.
    Archive,
    /// Одиночный файл: кладётся в папку `dir` под именем из `files[0]`.
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetBase {
    Models,
    NativeLibs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetStatus {
    Ok,
    /// Папки ресурса нет.
    Missing,
    /// Папка есть, но метки проверки нет: распаковано вручную или прервано.
    NotVerified,
    /// Метка от другого архива (другая версия или повреждённая загрузка).
    HashMismatch {
        found: String,
    },
    /// Метка верная, но файл внутри удалён.
    FileMissing(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("не удалось прочитать {}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ошибка в {}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("в {MANIFEST_FILE} нет ресурса `{0}`")]
    UnknownAsset(String),
    #[error("{name} не установлен ({status:?}) — запустите scripts\\download-models.ps1")]
    NotInstalled { name: String, status: AssetStatus },
}

impl Manifest {
    pub fn load(root: &Path) -> Result<Self, ManifestError> {
        let path = project_path(root, MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Read {
            path: path.clone(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| ManifestError::Parse { path, source })
    }
}

impl Manifest {
    /// Ресурс по `id` из `assets.json`.
    pub fn get(&self, id: &str) -> Result<&Asset, ManifestError> {
        self.assets
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| ManifestError::UnknownAsset(id.to_owned()))
    }
}

impl Asset {
    /// Путь к файлу ресурса, проверив, что ресурс установлен целиком.
    pub fn file(&self, root: &Path, config: &Config, name: &str) -> Result<PathBuf, ManifestError> {
        let location = self.location(root, config);
        match self.status(&location) {
            AssetStatus::Ok => Ok(location.join(name)),
            status => Err(ManifestError::NotInstalled {
                name: self.name.clone(),
                status,
            }),
        }
    }

    /// Абсолютный путь к распакованной папке ресурса.
    pub fn location(&self, root: &Path, config: &Config) -> PathBuf {
        let base = match self.base {
            AssetBase::Models => &config.paths.models,
            AssetBase::NativeLibs => &config.paths.native_libs,
        };
        config::resolve(root, base).join(&self.dir)
    }

    pub fn status(&self, location: &Path) -> AssetStatus {
        if !location.is_dir() {
            return AssetStatus::Missing;
        }
        let marker = match std::fs::read_to_string(location.join(MARKER_FILE)) {
            Ok(text) => text.trim().to_ascii_lowercase(),
            Err(_) => return AssetStatus::NotVerified,
        };
        if marker != self.sha256.to_ascii_lowercase() {
            return AssetStatus::HashMismatch { found: marker };
        }
        match self.files.iter().find(|f| !location.join(f).is_file()) {
            Some(missing) => AssetStatus::FileMissing(missing.clone()),
            None => AssetStatus::Ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = include_str!("../../../scripts/assets.json");

    fn asset() -> Asset {
        Asset {
            id: "test".into(),
            name: "Тест 1.0".into(),
            description: "тест".into(),
            url: "https://example.com/a.zip".into(),
            sha256: "ABCDEF".into(),
            size: 1,
            kind: AssetKind::Archive,
            base: AssetBase::Models,
            dir: "a".into(),
            required: true,
            build_only: false,
            files: vec!["conf/model.conf".into()],
        }
    }

    #[test]
    fn shipped_manifest_parses_and_is_sane() {
        let manifest: Manifest = serde_json::from_str(SHIPPED).unwrap();
        assert!(manifest.assets.iter().any(|a| a.required));
        for a in &manifest.assets {
            assert_eq!(
                a.sha256.len(),
                64,
                "{}: sha256 должен быть 64 hex-символа",
                a.id
            );
            assert!(a.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{}", a.id);
            assert!(a.url.starts_with("https://"), "{}", a.id);
            assert!(!a.files.is_empty(), "{}", a.id);
            if a.kind == AssetKind::File {
                assert_eq!(a.files.len(), 1, "{}: у файла ровно одно имя", a.id);
            }
        }
        let ids: std::collections::HashSet<_> = manifest.assets.iter().map(|a| &a.id).collect();
        assert_eq!(ids.len(), manifest.assets.len(), "id ресурсов не уникальны");
    }

    #[test]
    fn status_walks_through_all_states() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("a");
        let a = asset();

        assert_eq!(a.status(&loc), AssetStatus::Missing);

        std::fs::create_dir_all(loc.join("conf")).unwrap();
        assert_eq!(a.status(&loc), AssetStatus::NotVerified);

        std::fs::write(loc.join(MARKER_FILE), "123\n").unwrap();
        assert_eq!(
            a.status(&loc),
            AssetStatus::HashMismatch {
                found: "123".into()
            }
        );

        // Регистр и перевод строки в метке не важны.
        std::fs::write(loc.join(MARKER_FILE), "abcdef\r\n").unwrap();
        assert_eq!(
            a.status(&loc),
            AssetStatus::FileMissing("conf/model.conf".into())
        );

        std::fs::write(loc.join("conf/model.conf"), "").unwrap();
        assert_eq!(a.status(&loc), AssetStatus::Ok);
    }
}
