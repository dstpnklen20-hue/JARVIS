//! Окно-вопрос с кнопками (TaskDialog) для вопроса-разрешения.
//!
//! Окно живёт в своём потоке (TaskDialog модальный и крутит свой цикл
//! сообщений). Закрыть его можно извне — когда ответ пришёл голосом.

use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{HWND, LPARAM, S_OK, WPARAM};
use windows_sys::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDF_ALLOW_DIALOG_CANCELLATION, TDM_CLICK_BUTTON,
    TDN_CREATED, TDN_DESTROYED, TaskDialogIndirect,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, IDCANCEL, PostMessageW, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos,
};
use windows_sys::core::HRESULT;

use super::wide;
use crate::PlatformError;

/// Кнопка окна: свой номер и подпись.
#[derive(Debug, Clone)]
pub struct DialogButton {
    pub id: i32,
    pub label: String,
}

pub struct DialogSpec {
    pub title: String,
    pub heading: String,
    pub text: String,
    pub buttons: Vec<DialogButton>,
    /// Номер кнопки по умолчанию (Enter).
    pub default_button: i32,
}

/// Открытое окно. Удаление объекта окно не закрывает — для этого `close()`.
pub struct DialogHandle {
    hwnd: Arc<AtomicIsize>,
    thread: Option<JoinHandle<()>>,
}

impl DialogHandle {
    /// Закрыть окно, как будто нажали «Отмена»; `on_result` получит `None`.
    pub fn close(&mut self) {
        let hwnd = self.hwnd.load(Ordering::Acquire) as HWND;
        if !hwnd.is_null() {
            // SAFETY: PostMessageW только ставит сообщение в очередь окна; если
            // окно уже закрылось, вызов вернёт ошибку — нас это устраивает.
            unsafe {
                PostMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDCANCEL as WPARAM, 0);
            }
        }
        if let Some(thread) = self.thread.take() {
            // Не ждём: окно может закрыться позже, поток завершится сам.
            drop(thread);
        }
    }
}

/// Показывает окно. `on_result` вызывается в потоке окна с номером нажатой
/// кнопки или `None` (закрыто, отменено, ошибка).
pub fn show_dialog(
    spec: DialogSpec,
    on_result: impl FnOnce(Option<i32>) + Send + 'static,
) -> Result<DialogHandle, PlatformError> {
    let hwnd = Arc::new(AtomicIsize::new(0));
    let hwnd_for_thread = Arc::clone(&hwnd);
    let thread = std::thread::Builder::new()
        .name("jarvis-ask".into())
        .spawn(move || {
            let result = run_dialog(&spec, &hwnd_for_thread);
            hwnd_for_thread.store(0, Ordering::Release);
            on_result(result);
        })
        .map_err(|_| PlatformError::Win32 {
            call: "CreateThread",
            code: 0,
        })?;
    Ok(DialogHandle {
        hwnd,
        thread: Some(thread),
    })
}

fn run_dialog(spec: &DialogSpec, hwnd_slot: &AtomicIsize) -> Option<i32> {
    let title = wide(&spec.title);
    let heading = wide(&spec.heading);
    let text = wide(&spec.text);
    let labels: Vec<Vec<u16>> = spec.buttons.iter().map(|b| wide(&b.label)).collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = spec
        .buttons
        .iter()
        .zip(&labels)
        .map(|(b, label)| TASKDIALOG_BUTTON {
            nButtonID: b.id,
            pszButtonText: label.as_ptr(),
        })
        .collect();

    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: title.as_ptr(),
        pszMainInstruction: heading.as_ptr(),
        pszContent: text.as_ptr(),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: spec.default_button,
        pfCallback: Some(callback),
        lpCallbackData: std::ptr::from_ref(hwnd_slot) as isize,
        ..TASKDIALOGCONFIG::default()
    };
    let mut pressed = 0i32;
    // SAFETY: все строки и массив кнопок живут до конца вызова (вызов
    // синхронный); hwnd_slot живёт дольше окна (поток держит Arc); размер
    // структуры указан верно.
    let hr = unsafe {
        TaskDialogIndirect(
            &config,
            &mut pressed,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if hr != S_OK || pressed == IDCANCEL {
        return None;
    }
    Some(pressed)
}

unsafe extern "system" fn callback(
    hwnd: HWND,
    msg: u32,
    _w: WPARAM,
    _l: LPARAM,
    data: isize,
) -> HRESULT {
    // SAFETY: data — указатель на AtomicIsize из run_dialog; он жив, пока
    // открыто окно (см. run_dialog).
    let slot = unsafe { &*(data as *const AtomicIsize) };
    match msg as i32 {
        TDN_CREATED => {
            slot.store(hwnd as isize, Ordering::Release);
            // Поверх остальных окон: вопрос не должен потеряться за браузером.
            // SAFETY: hwnd — только что созданное окно этого потока.
            unsafe {
                SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
                SetForegroundWindow(hwnd);
            }
        }
        TDN_DESTROYED => slot.store(0, Ordering::Release),
        _ => {}
    }
    S_OK
}
