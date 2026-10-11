//! Мягкая остановка работающей копии извне: `jarvis --stop` взводит
//! именованное событие, демон ждёт его в отдельном потоке.

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};

use super::{OwnedHandle, last_error, wide};
use crate::PlatformError;

pub struct StopSignal(OwnedHandle);

fn event_name(name: &str) -> Vec<u16> {
    wide(&format!("Local\\{name}.Stop"))
}

/// Создаёт событие остановки (делает работающая копия).
pub fn create_stop_signal(name: &str) -> Result<StopSignal, PlatformError> {
    let name = event_name(name);
    // SAFETY: имя нуль-терминировано; событие со сбросом вручную, изначально
    // не взведено, атрибуты безопасности по умолчанию.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(last_error("CreateEventW"));
    }
    Ok(StopSignal(OwnedHandle(handle)))
}

impl StopSignal {
    /// Блокирует поток до сигнала остановки.
    pub fn wait(&self) -> Result<(), PlatformError> {
        // SAFETY: дескриптор события валиден, пока жив `self`.
        let result = unsafe { WaitForSingleObject(self.0.0, INFINITE) };
        if result == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(last_error("WaitForSingleObject"))
        }
    }
}

/// Просит работающую копию остановиться. `Ok(false)` — копия не запущена.
pub fn send_stop_signal(name: &str) -> Result<bool, PlatformError> {
    let name = event_name(name);
    // SAFETY: имя нуль-терминировано; запрашиваем только право взвести событие.
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if handle.is_null() {
        return Ok(false);
    }
    let handle = OwnedHandle(handle);
    // SAFETY: дескриптор только что открыт с правом EVENT_MODIFY_STATE.
    if unsafe { SetEvent(handle.0) } == 0 {
        return Err(last_error("SetEvent"));
    }
    Ok(true)
}
