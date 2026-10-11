//! Заглушки для не-Windows платформ: проект пока поддерживает только Windows.

use crate::{OsInfo, PlatformError};

pub fn os_info() -> Result<OsInfo, PlatformError> {
    Err(PlatformError::Unsupported)
}

pub fn is_elevated() -> Result<bool, PlatformError> {
    Err(PlatformError::Unsupported)
}
