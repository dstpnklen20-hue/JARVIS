//! Главное окно Джарвиса — рамка для страницы WebView2 (страницу строит
//! jarvis-app через wry, отдавая ей это окно через [`HasWindowHandle`]).
//!
//! Окно живёт в главном потоке, где крутится цикл сообщений трея: своего
//! цикла у него нет. Крестик разрушает окно; владелец узнаёт об этом из
//! [`AppWindow::is_open`] и освобождает страницу (WebView2 держит заметную
//! память — закрытое окно её не занимает).

use std::num::NonZeroIsize;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::CreateSolidBrush;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateIcon, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow,
    GWLP_USERDATA, GetWindowLongPtrW, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, IsIconic, IsWindow,
    LoadCursorW, MINMAXINFO, RegisterClassExW, SPI_GETWORKAREA, SW_RESTORE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, SystemParametersInfoW, WM_CLOSE, WM_DPICHANGED, WM_GETMINMAXINFO,
    WM_NCCREATE, WM_NCDESTROY, WM_SETICON, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

use super::{last_error, wide};
use crate::PlatformError;

const CLASS_NAME: &str = "JarvisMainWindow";
/// Тёмный заголовок окна (Windows 10 20H1+ / 11).
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// Цвет заголовка (Windows 11).
const DWMWA_CAPTION_COLOR: u32 = 35;

/// Что нужно знать окну о себе в процедуре окна.
struct WindowState {
    closed: Arc<AtomicBool>,
    /// Наименьший размер в логических пикселях (96 dpi).
    min_size: (i32, i32),
}

/// Параметры окна.
pub struct AppWindowSpec<'a> {
    pub title: &'a str,
    /// Размер области страницы в логических пикселях (при 100 % масштаба).
    pub size: (i32, i32),
    pub min_size: (i32, i32),
    /// Цвет фона до загрузки страницы, 0xRRGGBB.
    pub background: u32,
    /// Значок окна и панели задач: RGBA, квадрат `icon_size` × `icon_size`.
    pub icon_rgba: Option<(&'a [u8], u32)>,
}

/// Открытое окно. Удаление объекта закрывает окно; только для главного потока.
pub struct AppWindow {
    hwnd: HWND,
    closed: Arc<AtomicBool>,
    icon: HICON,
}

/// Главный поток рисует в настоящем разрешении экрана: иначе Windows
/// растягивает окно картинкой и страница выглядит мыльной. Только этот
/// поток — окна других потоков (вопрос-разрешение, ответ) остаются как были.
pub fn use_per_monitor_dpi() {
    // SAFETY: меняет режим DPI только текущего потока; предыдущий не нужен.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Создать и показать окно (в текущем потоке — у него должен быть цикл сообщений).
pub fn open_app_window(spec: &AppWindowSpec<'_>) -> Result<AppWindow, PlatformError> {
    if !register_class(spec.background) {
        return Err(last_error("RegisterClassExW"));
    }
    let closed = Arc::new(AtomicBool::new(false));
    let state = Box::new(WindowState {
        closed: Arc::clone(&closed),
        min_size: spec.min_size,
    });
    let raw = Box::into_raw(state);
    let class = wide(CLASS_NAME);
    let title = wide(spec.title);
    // SAFETY: класс зарегистрирован; строки живут до конца вызова; `raw`
    // окно заберёт в WM_NCCREATE и освободит в WM_NCDESTROY.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            spec.size.0,
            spec.size.1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            raw.cast(),
        )
    };
    if hwnd.is_null() {
        let error = last_error("CreateWindowExW");
        // Окно не создалось — WM_NCCREATE мог не прийти: проверить, забрало ли
        // окно состояние, нельзя, поэтому состояние остаётся (утечка байтов
        // лучше двойного освобождения).
        return Err(error);
    }
    let icon = spec
        .icon_rgba
        .map(|(rgba, size)| make_icon(rgba, size))
        .unwrap_or(std::ptr::null_mut());
    // SAFETY: hwnd — только что созданное окно этого потока; указатели на
    // значения атрибутов живут до конца вызова.
    unsafe {
        let dark: i32 = 1;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast(),
            size_of::<i32>() as u32,
        );
        // COLORREF — 0x00BBGGRR.
        let caption = bgr(spec.background);
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            (&raw const caption).cast(),
            size_of::<u32>() as u32,
        );
        if !icon.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, icon as LPARAM);
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, icon as LPARAM);
        }
    }
    place_centered(hwnd, spec.size);
    // SAFETY: окно этого потока.
    unsafe {
        // Первый показ окна в процессе Windows может заменить режимом запуска
        // процесса (например, свёрнутым) — тогда развернуть вторым вызовом.
        ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    Ok(AppWindow { hwnd, closed, icon })
}

impl AppWindow {
    /// Окно не закрыли крестиком.
    pub fn is_open(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }

    /// Развернуть, если свёрнуто, и вывести вперёд.
    pub fn bring_to_front(&self) {
        if !self.is_open() {
            return;
        }
        // SAFETY: окно этого потока, ещё не разрушено (проверено выше).
        unsafe {
            if IsIconic(self.hwnd) != 0 {
                ShowWindow(self.hwnd, SW_RESTORE);
            }
            SetForegroundWindow(self.hwnd);
            SetFocus(self.hwnd);
        }
    }
}

impl Drop for AppWindow {
    fn drop(&mut self) {
        // SAFETY: окно и значок созданы этим объектом в этом потоке;
        // IsWindow защищает от повторного разрушения уже закрытого окна.
        unsafe {
            if self.is_open() && IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
            if !self.icon.is_null() {
                DestroyIcon(self.icon);
            }
        }
    }
}

impl HasWindowHandle for AppWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        if !self.is_open() {
            return Err(HandleError::Unavailable);
        }
        let hwnd = NonZeroIsize::new(self.hwnd as isize).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
        // SAFETY: окно живо, пока жив `self` (Drop разрушает его последним),
        // а заимствование привязано к `self`.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn bgr(rgb: u32) -> u32 {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

fn register_class(background: u32) -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| {
        let class = wide(CLASS_NAME);
        // SAFETY: структура заполнена полностью, имя класса живёт до конца
        // вызова; кисть фона живёт всё время работы программы (класс один).
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
                hbrBackground: CreateSolidBrush(bgr(background)),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc) != 0
        }
    })
}

/// Значок из RGBA (строки сверху вниз).
fn make_icon(rgba: &[u8], size: u32) -> HICON {
    let pixels = (size * size) as usize;
    if rgba.len() != pixels * 4 {
        return std::ptr::null_mut();
    }
    // 32-битный цвет в порядке BGRA; маска AND — нули (прозрачность из альфы),
    // строки маски выровнены до 16 бит.
    let bgra: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [b, g, r, a])
        .collect();
    let mask_row = size.div_ceil(16) * 2;
    let mask = vec![0u8; (mask_row * size) as usize];
    // SAFETY: буферы нужного размера живут до конца вызова (Windows копирует).
    unsafe {
        CreateIcon(
            GetModuleHandleW(std::ptr::null()),
            size as i32,
            size as i32,
            1,
            32,
            mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
}

/// Размер области страницы под DPI монитора и окно — по центру рабочей области.
fn place_centered(hwnd: HWND, (width, height): (i32, i32)) {
    // SAFETY: окно этого потока; структуры валидны для записи.
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = |v: i32| v * dpi as i32 / 96;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: scale(width),
            bottom: scale(height),
        };
        AdjustWindowRectExForDpi(&mut frame, WS_OVERLAPPEDWINDOW, 0, 0, dpi);
        let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
        let mut area = RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut area).cast(), 0);
        let x = area.left + ((area.right - area.left - w) / 2).max(0);
        let y = area.top + ((area.bottom - area.top - h) / 2).max(0);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Состояние окна (если уже создано).
fn state<'a>(hwnd: HWND) -> Option<&'a WindowState> {
    // SAFETY: в GWLP_USERDATA лежит либо 0, либо указатель на WindowState,
    // сохранённый в WM_NCCREATE и живущий до WM_NCDESTROY; доступ — только
    // из потока окна.
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
        ptr.as_ref()
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: для WM_NCCREATE lParam — указатель на CREATESTRUCTW,
            // lpCreateParams — Box<WindowState> из open_app_window.
            unsafe {
                let cs = &*(l as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        WM_GETMINMAXINFO => {
            if let Some(st) = state(hwnd) {
                // SAFETY: для WM_GETMINMAXINFO lParam — указатель на MINMAXINFO;
                // окно этого потока.
                unsafe {
                    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                    let info = &mut *(l as *mut MINMAXINFO);
                    info.ptMinTrackSize.x = st.min_size.0 * dpi / 96;
                    info.ptMinTrackSize.y = st.min_size.1 * dpi / 96;
                }
            }
            0
        }
        WM_DPICHANGED => {
            // Другой монитор — размер, который предлагает Windows.
            // SAFETY: для WM_DPICHANGED lParam — указатель на RECT.
            unsafe {
                let rc = &*(l as *const RECT);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
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
        WM_NCDESTROY => {
            // SAFETY: указатель на WindowState из WM_NCCREATE (или 0);
            // освобождается ровно один раз.
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !ptr.is_null() {
                    let st = Box::from_raw(ptr);
                    st.closed.store(true, Ordering::Release);
                }
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        // SAFETY: остальное — стандартная обработка.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
