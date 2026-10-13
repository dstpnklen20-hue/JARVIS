//! Сигналы работающей копии извне — именованные события:
//! - остановка: `jarvis --stop` взводит событие, демон ждёт его в отдельном потоке;
//! - «покажи окно»: повторный запуск Джарвиса открывает окно уже работающего.

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};

use super::{OwnedHandle, last_error, wide};
use crate::PlatformError;

/// Событие, которое ждёт работающая копия.
pub struct StopSignal(OwnedHandle);

const STOP: &str = "Stop";
const SHOW: &str = "Show";

fn event_name(name: &str, kind: &str) -> Vec<u16> {
    wide(&format!("Local\\{name}.{kind}"))
}

/// `manual_reset`: остановка остаётся взведённой навсегда, «покажи окно» —
/// сбрасывается после каждого ожидания (окно можно просить много раз).
fn create(name: &str, kind: &str, manual_reset: bool) -> Result<StopSignal, PlatformError> {
    let name = event_name(name, kind);
    // SAFETY: имя нуль-терминировано; событие изначально не взведено,
    // атрибуты безопасности по умолчанию.
    let handle =
        unsafe { CreateEventW(std::ptr::null(), i32::from(manual_reset), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(last_error("CreateEventW"));
    }
    Ok(StopSignal(OwnedHandle(handle)))
}

fn send(name: &str, kind: &str) -> Result<bool, PlatformError> {
    let name = event_name(name, kind);
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

/// Создаёт событие остановки (делает работающая копия).
pub fn create_stop_signal(name: &str) -> Result<StopSignal, PlatformError> {
    create(name, STOP, true)
}

/// Создаёт событие «покажи окно» (делает работающая копия).
pub fn create_show_signal(name: &str) -> Result<StopSignal, PlatformError> {
    create(name, SHOW, false)
}

impl StopSignal {
    /// Блокирует поток до сигнала.
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
    send(name, STOP)
}

/// Просит работающую копию показать окно. `Ok(false)` — копия не запущена.
pub fn send_show_signal(name: &str) -> Result<bool, PlatformError> {
    send(name, SHOW)
}
