//! Окно с текстом ответа нейросети: в правом нижнем углу, поверх остальных,
//! не забирает фокус (не мешает печатать), текст обновляется по мере ответа.
//!
//! Окно живёт в своём потоке со своим циклом сообщений. Ядро меняет текст
//! через [`TextWindow::set_text`]: строка кладётся в общую ячейку, окну
//! уходит сообщение — оно перечитывает ячейку в своём потоке.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL,
    GetSysColorBrush, HFONT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{EM_SCROLLCARET, EM_SETSEL};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GWLP_USERDATA, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IDC_ARROW, LoadCursorW, MSG, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassExW, SPI_GETWORKAREA, SW_SHOWNOACTIVATE, SendMessageW, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_APP, WM_CLOSE,
    WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WS_CAPTION,
    WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_VISIBLE,
    WS_VSCROLL,
};

use super::wide;
use crate::PlatformError;

const CLASS_NAME: &str = "JarvisTextWindow";
/// «Перечитай текст».
const WM_TEXT_CHANGED: u32 = WM_APP + 1;
const WIDTH: i32 = 480;
const HEIGHT: i32 = 300;
const MARGIN: i32 = 16;

/// Общее у ядра и окна.
struct Shared {
    text: Mutex<String>,
    hwnd: AtomicIsize,
}

/// Состояние окна в его потоке (по указателю из GWLP_USERDATA).
struct WindowState {
    shared: Arc<Shared>,
    edit: HWND,
    font: HFONT,
}

/// Открытое окно. Удаление объекта окно закрывает.
pub struct TextWindow {
    shared: Arc<Shared>,
}

impl TextWindow {
    /// Заменить текст окна (с `\n` — строки).
    pub fn set_text(&self, text: &str) {
        if let Ok(mut slot) = self.shared.text.lock() {
            *slot = text.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        post(&self.shared, WM_TEXT_CHANGED);
    }

    /// Окно ещё открыто (его не закрыли крестиком).
    pub fn is_open(&self) -> bool {
        self.shared.hwnd.load(Ordering::Acquire) != 0
    }

    pub fn close(&self) {
        post(&self.shared, WM_CLOSE);
    }
}

impl Drop for TextWindow {
    fn drop(&mut self) {
        self.close();
    }
}

fn post(shared: &Shared, msg: u32) {
    let hwnd = shared.hwnd.load(Ordering::Acquire) as HWND;
    if !hwnd.is_null() {
        // SAFETY: только ставит сообщение в очередь окна; если окно уже
        // закрыто, вызов вернёт ошибку — это нас устраивает.
        unsafe {
            PostMessageW(hwnd, msg, 0, 0);
        }
    }
}

/// Показать окно с заголовком и текстом.
pub fn show_text_window(title: &str, text: &str) -> Result<TextWindow, PlatformError> {
    let shared = Arc::new(Shared {
        text: Mutex::new(text.replace('\n', "\r\n")),
        hwnd: AtomicIsize::new(0),
    });
    let for_thread = Arc::clone(&shared);
    let title = title.to_owned();
    std::thread::Builder::new()
        .name("jarvis-text".into())
        .spawn(move || run_window(&title, for_thread))
        .map_err(|_| PlatformError::Win32 {
            call: "CreateThread",
            code: 0,
        })?;
    Ok(TextWindow { shared })
}

fn register_class() -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| {
        let class = wide(CLASS_NAME);
        // SAFETY: структура заполнена полностью, строка имени класса живёт до
        // конца вызова (Windows копирует имя), процедура окна — функция ниже.
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc) != 0
        }
    })
}

fn run_window(title: &str, shared: Arc<Shared>) {
    // Окно не создалось — просто не будет окна: ответ всё равно звучит.
    if !register_class() {
        return;
    }
    // Правый нижний угол рабочей области (без панели задач).
    let mut area = RECT {
        left: 0,
        top: 0,
        right: 1280,
        bottom: 720,
    };
    // SAFETY: area — валидная структура RECT для записи.
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut area).cast(), 0);
    }
    let class = wide(CLASS_NAME);
    let title = wide(title);
    // Указатель на Arc передаётся окну и забирается в WM_NCDESTROY.
    let raw = Arc::into_raw(Arc::clone(&shared)) as *mut core::ffi::c_void;
    // SAFETY: класс зарегистрирован; строки живут до конца вызова; `raw`
    // окно сохранит в WM_NCCREATE и освободит в WM_NCDESTROY.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            area.right - WIDTH - MARGIN,
            area.bottom - HEIGHT - MARGIN,
            WIDTH,
            HEIGHT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            raw,
        )
    };
    if hwnd.is_null() {
        // Окно не создалось — WM_NCDESTROY не придёт, вернуть Arc самим.
        // SAFETY: raw получен из Arc::into_raw выше и ещё не возвращён.
        drop(unsafe { Arc::from_raw(raw as *const Shared) });
        return;
    }
    shared.hwnd.store(hwnd as isize, Ordering::Release);
    // SAFETY: hwnd — только что созданное окно этого потока.
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    let mut msg: MSG = MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
    };
    // SAFETY: стандартный цикл сообщений потока окна; msg — валидная структура.
    unsafe {
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    shared.hwnd.store(0, Ordering::Release);
}

/// Состояние окна (если уже создано).
fn state<'a>(hwnd: HWND) -> Option<&'a mut WindowState> {
    // SAFETY: в GWLP_USERDATA лежит либо 0, либо указатель на WindowState,
    // созданный в WM_CREATE и живущий до WM_NCDESTROY; доступ — только из
    // потока окна.
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        ptr.as_mut()
    }
}

fn show_shared_text(st: &WindowState) {
    let text = st.shared.text.lock().map(|t| wide(&t)).unwrap_or_default();
    // SAFETY: edit — дочернее окно этого потока; строка живёт до конца вызова.
    unsafe {
        SetWindowTextW(st.edit, text.as_ptr());
        // Курсор в конец — видно последние строки ответа.
        SendMessageW(st.edit, EM_SETSEL, usize::MAX, -1);
        SendMessageW(st.edit, EM_SCROLLCARET, 0, 0);
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: для WM_NCCREATE lParam — указатель на CREATESTRUCTW,
            // lpCreateParams — наш Arc<Shared> из run_window (забираем его
            // ровно один раз). Состояние окна живёт до WM_NCDESTROY.
            unsafe {
                let cs = &*(l as *const CREATESTRUCTW);
                let shared = Arc::from_raw(cs.lpCreateParams as *const Shared);
                let st = Box::new(WindowState {
                    shared,
                    edit: std::ptr::null_mut(),
                    font: std::ptr::null_mut(),
                });
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        WM_CREATE => {
            let Some(st) = state(hwnd) else {
                return -1;
            };
            // SAFETY: дочернее окно и шрифт создаются для этого окна в его
            // потоке; освобождаются в WM_NCDESTROY.
            unsafe {
                let edit_class = wide("EDIT");
                let edit = CreateWindowExW(
                    0,
                    edit_class.as_ptr(),
                    std::ptr::null(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_VSCROLL
                        | ES_MULTILINE as u32
                        | ES_READONLY as u32
                        | ES_AUTOVSCROLL as u32,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                );
                let face = wide("Segoe UI");
                let font = CreateFontW(
                    -17,
                    0,
                    0,
                    0,
                    FW_NORMAL as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    0,
                    0,
                    CLEARTYPE_QUALITY as u32,
                    0,
                    face.as_ptr(),
                );
                SendMessageW(edit, WM_SETFONT, font as WPARAM, 1);
                st.edit = edit;
                st.font = font;
            }
            show_shared_text(st);
            0
        }
        WM_SIZE => {
            if let Some(st) = state(hwnd) {
                let mut rc = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                // SAFETY: окна этого потока; rc — валидная структура.
                unsafe {
                    GetClientRect(hwnd, &mut rc);
                    MoveWindow(st.edit, 6, 6, rc.right - 12, rc.bottom - 12, 1);
                }
            }
            0
        }
        WM_TEXT_CHANGED => {
            if let Some(st) = state(hwnd) {
                show_shared_text(st);
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: закрываем своё окно в его потоке.
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: завершить цикл сообщений этого потока.
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => {
            // SAFETY: указатель на WindowState из WM_NCCREATE (или 0);
            // освобождается ровно один раз.
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !ptr.is_null() {
                    let st = Box::from_raw(ptr);
                    st.shared.hwnd.store(0, Ordering::Release);
                    if !st.font.is_null() {
                        DeleteObject(st.font);
                    }
                }
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        // SAFETY: остальное — стандартная обработка.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
