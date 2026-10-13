//! Реализация для Windows через `windows-sys`.

mod actions;
mod browser;
mod dialog;
mod event_loop;
mod hotkey;
mod instance;
mod keys;
mod power;
mod process;
mod stop;
mod system;

pub use actions::{MediaKey, media_key, open_url};
pub use browser::{activate_browser_tab, focus_app, focus_browser, process_running};
pub use dialog::{DialogButton, DialogHandle, DialogSpec, show_dialog};
pub use event_loop::{
    LoopEvent, Waker, current_waker, install_ctrl_handler, notify_shutdown_complete,
    run_message_loop,
};
pub use hotkey::{HotkeyGuard, parse_hotkey, register_hotkey};
pub use instance::acquire_single_instance;
pub use keys::{press_keys, release_all_keys};
pub use power::{Power, power, set_volume};
pub use process::{RunEnd, run_and_wait, spawn_detached};
pub use stop::{StopSignal, create_stop_signal, send_stop_signal};
pub use system::{is_elevated, os_info};

fn last_error(call: &'static str) -> crate::PlatformError {
    crate::PlatformError::Win32 {
        call,
        code: std::io::Error::last_os_error()
            .raw_os_error()
            .map_or(0, |c| c as u32),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Дескриптор, который закрывается при выходе из области видимости.
struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: дескриптор получен от WinAPI, принадлежит только этой
        // структуре и закрывается ровно один раз.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

// SAFETY: дескрипторы ядра Windows (токен, мьютекс) можно использовать и
// закрывать из любого потока процесса.
unsafe impl Send for OwnedHandle {}
