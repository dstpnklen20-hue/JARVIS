//! Защита от запуска второй копии: именованный мьютекс в сессии пользователя.

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

use super::{OwnedHandle, last_error, wide};
use crate::{InstanceGuard, PlatformError};

pub fn acquire_single_instance(name: &str) -> Result<Option<InstanceGuard>, PlatformError> {
    // Local\ — мьютекс виден только в сессии текущего пользователя.
    let name = wide(&format!("Local\\{name}"));
    // SAFETY: имя нуль-терминировано и живёт до конца вызова; атрибуты
    // безопасности по умолчанию (null); начальным владельцем не становимся.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(last_error("CreateMutexW"));
    }
    let handle = OwnedHandle(handle);
    // SAFETY: читаем код ошибки сразу после CreateMutexW в том же потоке.
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        // Наш дескриптор закроется здесь; мьютекс остаётся за первой копией.
        Ok(None)
    } else {
        Ok(Some(InstanceGuard {
            _handle: Box::new(handle),
        }))
    }
}
