use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("микрофон «{0}» не найден")]
    DeviceNotFound(String),
    #[error("в системе нет микрофона по умолчанию")]
    NoDefaultInput,
    #[error("в системе нет устройства вывода звука по умолчанию")]
    NoDefaultOutput,
    #[error("ошибка аудиоустройства: {0}")]
    Device(String),
    #[error("формат отсчётов {0} не поддерживается")]
    UnsupportedFormat(String),
    #[error("ошибка ресемплинга: {0}")]
    Resample(String),
    #[error("не удалось загрузить модель {what} ({})", path.display())]
    ModelLoad { what: &'static str, path: PathBuf },
    #[error(transparent)]
    Assets(#[from] jarvis_core::assets::ManifestError),
    #[error("не удалось прочитать {}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("не удалось записать {}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("WAV {}: {reason}", path.display())]
    Wav { path: PathBuf, reason: String },
    #[error("звуковой пакет {}: {reason}", path.display())]
    Pack { path: PathBuf, reason: String },
    #[error("ошибка в {}", path.display())]
    PackToml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("образец голоса {}: {reason}", path.display())]
    Voiceprint { path: PathBuf, reason: String },
}

/// Ошибки cpal бывают разных типов; для пользователя важен текст.
pub(crate) fn device_err(e: impl std::fmt::Display) -> AudioError {
    AudioError::Device(e.to_string())
}
