//! Ключ API — только в хранилище секретов ОС (Windows: Диспетчер учётных
//! данных), никогда в коде, конфигах и логах.

use crate::LlmError;

const SERVICE: &str = "jarvis";
const ACCOUNT: &str = "anthropic-api-key";

fn entry() -> Result<keyring::Entry, LlmError> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(LlmError::Keyring)
}

/// Ключ, если сохранён.
pub fn api_key() -> Result<Option<String>, LlmError> {
    read(&entry()?)
}

fn read(entry: &keyring::Entry) -> Result<Option<String>, LlmError> {
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(LlmError::Keyring(e)),
    }
}

pub fn save_api_key(key: &str) -> Result<(), LlmError> {
    entry()?.set_password(key).map_err(LlmError::Keyring)
}

/// `true` — ключ был и удалён.
pub fn forget_api_key() -> Result<bool, LlmError> {
    delete(&entry()?)
}

fn delete(entry: &keyring::Entry) -> Result<bool, LlmError> {
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(LlmError::Keyring(e)),
    }
}

/// Похоже ли на ключ Anthropic (защита от вставки не того текста).
pub fn looks_like_key(key: &str) -> bool {
    let key = key.trim();
    key.starts_with("sk-ant-") && key.len() >= 40 && !key.contains(char::is_whitespace)
}

/// Ключ сервера-посредника: формат у каждого свой — только латиница, цифры и
/// знаки, без пробелов, не короче 20 символов.
pub fn looks_like_server_key(key: &str) -> bool {
    let key = key.trim();
    key.len() >= 20 && key.chars().all(|c| c.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shape_is_checked() {
        assert!(looks_like_key(&format!("sk-ant-api03-{}", "x".repeat(40))));
        assert!(!looks_like_key("hello"));
        assert!(!looks_like_key(&format!("sk-cvc-{}", "x".repeat(40))));
        assert!(looks_like_server_key(&format!("sk-cvc-{}", "x".repeat(40))));
        assert!(!looks_like_server_key("sk-смс-1234567890123456789"));
        assert!(!looks_like_server_key("short"));
        assert!(!looks_like_key(&format!("sk-ant- {}", "x".repeat(40))));
    }

    /// Настоящее хранилище Windows, отдельная тестовая запись (не ключ Джарвиса).
    #[test]
    #[ignore = "пишет в Диспетчер учётных данных Windows (и сразу удаляет)"]
    fn os_store_roundtrip() {
        let entry = keyring::Entry::new("jarvis-test", "roundtrip").unwrap();
        let _ = delete(&entry);
        assert_eq!(read(&entry).unwrap(), None);
        entry.set_password("sk-ant-test-value").unwrap();
        assert_eq!(read(&entry).unwrap().as_deref(), Some("sk-ant-test-value"));
        assert!(delete(&entry).unwrap());
        assert!(!delete(&entry).unwrap());
        assert_eq!(read(&entry).unwrap(), None);
    }
}
