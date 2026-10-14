//! Окно ответа нейросети в стиле HUD Джарвиса: тёмное, в правом нижнем углу,
//! поверх остальных, не забирает фокус (не мешает печатать). Заголовок —
//! реактор и «J.A.R.V.I.S.», под ним вопрос и ответ; пока ответ идёт, по линии
//! под заголовком бежит световой отрезок. Высота растёт с ответом (до
//! предела, дальше — прокрутка колесом). Перетаскивается за заголовок,
//! закрывается крестиком.
//!
//! Окно живёт в своём потоке со своим циклом сообщений и рисует себя само
//! (GDI, с буфером — без мерцания при потоковом ответе). Ядро меняет
//! содержимое через [`AnswerWindow::set`]: оно кладётся в общую ячейку, окну
//! уходит сообщение — оно перечитывает ячейку в своём потоке.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateDIBSection, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT,
    DT_EDITCONTROL, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect, GetDC, HBITMAP, HDC, HFONT,
    IntersectClipRect, InvalidateRect, PAINTSTRUCT, ReleaseDC, RestoreDC, SaveDC, ScreenToClient,
    SelectObject, SetBkMode, SetTextCharacterExtra, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    HTCAPTION, HTCLIENT, IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW, MA_NOACTIVATE, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, SPI_GETWORKAREA, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOZORDER, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, TranslateMessage, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_NCCREATE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::wide;
use crate::PlatformError;

const CLASS_NAME: &str = "JarvisAnswerWindow";
/// «Перечитай содержимое».
const WM_CONTENT_CHANGED: u32 = WM_APP + 1;
const ANIMATION_TIMER: usize = 1;
const ANIMATION_MS: u32 = 33;

// Размеры в логических пикселях (100 % масштаба).
const WIDTH: i32 = 440;
const MIN_HEIGHT: i32 = 150;
const MAX_HEIGHT: i32 = 480;
const MARGIN: i32 = 16;
const HEADER: i32 = 46;
const PAD: i32 = 20;
const LOGO: i32 = 26;
/// Отступ текста от полосы слева.
const BAR_GAP: i32 = 12;
const CLOSE: i32 = 30;
const WHEEL_STEP: i32 = 48;

// Палитра главного окна (ui/index.html), 0xRRGGBB.
const BG: u32 = 0x05090E;
const HEADER_BG: u32 = 0x08111A;
const LINE: u32 = 0x182532;
const ACCENT: u32 = 0x1E8CF0;
const ACCENT_HI: u32 = 0x7FC4FF;
const TEXT: u32 = 0xD6E4EE;
const QUESTION: u32 = 0x9DB0C0;
const MUTED: u32 = 0x6C7F90;
const DIM: u32 = 0x2A3A4A;
const FAIL: u32 = 0xE05A5A;
const BORDER: u32 = 0x1D3A55;

/// Что сейчас с ответом — от этого надпись в заголовке и анимация.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerStatus {
    /// Вопрос ушёл, текста ещё нет.
    Thinking,
    /// Текст приходит.
    Answering,
    Done,
    Failed,
}

impl AnswerStatus {
    fn label(self) -> &'static str {
        match self {
            AnswerStatus::Thinking => "АНАЛИЗ ЗАПРОСА",
            AnswerStatus::Answering => "ОТВЕЧАЮ",
            AnswerStatus::Done => "ГОТОВО",
            AnswerStatus::Failed => "СБОЙ",
        }
    }

    fn animated(self) -> bool {
        matches!(self, AnswerStatus::Thinking | AnswerStatus::Answering)
    }
}

/// Содержимое окна.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerView {
    pub question: String,
    pub answer: String,
    pub status: AnswerStatus,
}

/// Значок реактора для заголовка: по стороне в пикселях — RGBA
/// (без предумножения), сторона × сторона.
pub type LogoPainter = fn(u32) -> Vec<u8>;

/// Общее у ядра и окна.
struct Shared {
    view: Mutex<AnswerView>,
    hwnd: AtomicIsize,
}

/// Открытое окно. Удаление объекта окно закрывает.
pub struct AnswerWindow {
    shared: Arc<Shared>,
}

impl AnswerWindow {
    /// Заменить содержимое.
    pub fn set(&self, view: AnswerView) {
        if let Ok(mut slot) = self.shared.view.lock() {
            *slot = view;
        }
        post(&self.shared, WM_CONTENT_CHANGED);
    }

    /// Сменить только состояние (текст тот же).
    pub fn set_status(&self, status: AnswerStatus) {
        if let Ok(mut slot) = self.shared.view.lock() {
            slot.status = status;
        }
        post(&self.shared, WM_CONTENT_CHANGED);
    }

    /// Окно ещё открыто (его не закрыли крестиком).
    pub fn is_open(&self) -> bool {
        self.shared.hwnd.load(Ordering::Acquire) != 0
    }

    pub fn close(&self) {
        post(&self.shared, WM_CLOSE);
    }
}

impl Drop for AnswerWindow {
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

/// Показать окно ответа.
pub fn show_answer_window(
    view: AnswerView,
    logo: Option<LogoPainter>,
) -> Result<AnswerWindow, PlatformError> {
    let shared = Arc::new(Shared {
        view: Mutex::new(view),
        hwnd: AtomicIsize::new(0),
    });
    let for_thread = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("jarvis-answer".into())
        .spawn(move || run_window(for_thread, logo))
        .map_err(|_| PlatformError::Win32 {
            call: "CreateThread",
            code: 0,
        })?;
    Ok(AnswerWindow { shared })
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
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                // Фон рисуем сами (WM_ERASEBKGND) — без белой вспышки.
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc) != 0
        }
    })
}

/// Шрифты под текущий масштаб.
struct Fonts {
    title: HFONT,
    label: HFONT,
    question: HFONT,
    answer: HFONT,
}

impl Fonts {
    fn new(scale: Scale) -> Self {
        Self {
            title: font("Bahnschrift SemiBold", scale.px(15), FW_SEMIBOLD),
            label: font("Bahnschrift", scale.px(11), FW_NORMAL),
            question: font("Segoe UI", scale.px(14), FW_NORMAL),
            answer: font("Segoe UI", scale.px(15), FW_NORMAL),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.title, self.label, self.question, self.answer] {
            delete_object(f);
        }
    }
}

/// Реактор в заголовке: готовая картинка с предумноженной прозрачностью.
struct Logo {
    bitmap: HBITMAP,
    size: i32,
}

impl Drop for Logo {
    fn drop(&mut self) {
        delete_object(self.bitmap);
    }
}

#[derive(Debug, Clone, Copy)]
struct Scale(u32);

impl Scale {
    fn px(self, v: i32) -> i32 {
        (v as i64 * self.0 as i64 / 96) as i32
    }
}

/// Раскладка содержимого (в физических пикселях, от верха области текста).
#[derive(Debug, Default, Clone, Copy)]
struct Layout {
    question_h: i32,
    answer_h: i32,
    /// Полная высота содержимого под заголовком.
    content_h: i32,
}

/// Состояние окна в его потоке (по указателю из GWLP_USERDATA).
struct WindowState {
    shared: Arc<Shared>,
    view: AnswerView,
    logo_painter: Option<LogoPainter>,
    scale: Scale,
    fonts: Option<Fonts>,
    logo: Option<Logo>,
    layout: Layout,
    /// Прокрутка содержимого вниз, пикселей.
    scroll: i32,
    /// Следовать за концом ответа (пока человек не прокрутил вверх).
    follow: bool,
    started: Instant,
    animating: bool,
}

fn run_window(shared: Arc<Shared>, logo: Option<LogoPainter>) {
    // Окно не создалось — просто не будет окна: ответ всё равно звучит.
    if !register_class() {
        return;
    }
    // SAFETY: меняет режим DPI только этого потока — окно рисуется чётко
    // при любом масштабе экрана.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let view = shared
        .view
        .lock()
        .map(|v| v.clone())
        .unwrap_or_else(|e| e.into_inner().clone());
    let state = Box::new(WindowState {
        shared: Arc::clone(&shared),
        view,
        logo_painter: logo,
        scale: Scale(96),
        fonts: None,
        logo: None,
        layout: Layout::default(),
        scroll: 0,
        follow: true,
        started: Instant::now(),
        animating: false,
    });
    let class = wide(CLASS_NAME);
    let title = wide("Джарвис — ответ");
    // Состояние передаётся окну и забирается в WM_NCDESTROY.
    let raw = Box::into_raw(state) as *mut core::ffi::c_void;
    // SAFETY: класс зарегистрирован; строки живут до конца вызова; `raw`
    // окно сохранит в WM_NCCREATE и освободит в WM_NCDESTROY. Размер и место
    // задаются в WM_CREATE, когда известен масштаб.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            WIDTH,
            MIN_HEIGHT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            raw,
        )
    };
    if hwnd.is_null() {
        // Состояние освобождено в WM_NCDESTROY (окно разрушается и при
        // отказе в WM_CREATE); класс зарегистрирован выше, так что до
        // WM_NCCREATE создание не обрывается.
        return;
    }
    shared.hwnd.store(hwnd as isize, Ordering::Release);
    // SAFETY: hwnd — только что созданное окно этого потока; атрибуты DWM
    // (скругление, цвет рамки) — Windows 11, на старых вызов просто не сработает.
    unsafe {
        let corners = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&raw const corners).cast(),
            size_of::<i32>() as u32,
        );
        let border = colorref(BORDER);
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&raw const border).cast(),
            size_of::<u32>() as u32,
        );
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    let mut msg: MSG = MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: POINT { x: 0, y: 0 },
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
    // SAFETY: в GWLP_USERDATA лежит либо 0, либо указатель на WindowState из
    // WM_NCCREATE, живущий до WM_NCDESTROY; доступ — только из потока окна.
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        ptr.as_mut()
    }
}

/// 0xRRGGBB → COLORREF (0x00BBGGRR).
fn colorref(rgb: u32) -> u32 {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

fn font(face: &str, height: i32, weight: u32) -> HFONT {
    let face = wide(face);
    // SAFETY: строка имени живёт до конца вызова; шрифт освобождает Fonts.
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            face.as_ptr(),
        )
    }
}

fn delete_object<T>(object: *mut T) {
    if !object.is_null() {
        // SAFETY: объект GDI создан этим окном и больше нигде не выбран.
        unsafe {
            DeleteObject(object.cast());
        }
    }
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn fill(dc: HDC, r: RECT, rgb: u32) {
    // SAFETY: dc — контекст рисования этого окна; кисть освобождается сразу.
    unsafe {
        let brush = CreateSolidBrush(colorref(rgb));
        FillRect(dc, &r, brush);
        DeleteObject(brush);
    }
}

/// Нарисовать (или с `DT_CALCRECT` — измерить) текст; возвращает высоту.
fn draw_text(
    dc: HDC,
    f: HFONT,
    rgb: u32,
    spacing: i32,
    text: &str,
    r: &mut RECT,
    flags: u32,
) -> i32 {
    let text = wide(text);
    // SAFETY: dc и шрифт — этого окна; строка с нулём в конце живёт до конца
    // вызова (длина −1); прежний шрифт возвращается на место.
    unsafe {
        let old = SelectObject(dc, f);
        SetTextColor(dc, colorref(rgb));
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextCharacterExtra(dc, spacing);
        let h = DrawTextW(dc, text.as_ptr(), -1, r, flags | DT_NOPREFIX);
        SetTextCharacterExtra(dc, 0);
        SelectObject(dc, old);
        h
    }
}

/// Перенос по словам, как в поле ввода (длинное слово тоже переносится).
const WRAP: u32 = DT_WORDBREAK | DT_EDITCONTROL;

impl WindowState {
    fn apply_dpi(&mut self, dpi: u32) {
        self.scale = Scale(dpi.max(96));
        self.fonts = Some(Fonts::new(self.scale));
        self.logo = self
            .logo_painter
            .and_then(|paint| make_logo(paint, self.scale.px(LOGO)));
    }

    /// Ширина колонки текста (физические пиксели).
    fn text_width(&self) -> i32 {
        let s = self.scale;
        s.px(WIDTH) - 2 * s.px(PAD) - s.px(BAR_GAP)
    }

    fn answer_text(&self) -> String {
        match self.view.status {
            AnswerStatus::Thinking if self.view.answer.trim().is_empty() => {
                let dots = (self.started.elapsed().as_millis() / 400 % 4) as usize;
                format!("Обрабатываю запрос{}", ".".repeat(dots))
            }
            _ => self.view.answer.replace("\r\n", "\n").replace('\n', "\r\n"),
        }
    }

    /// Пересчитать высоты текста; вернуть нужную высоту окна.
    fn relayout(&mut self, hwnd: HWND) -> i32 {
        let Some(fonts) = &self.fonts else {
            return self.scale.px(MIN_HEIGHT);
        };
        let s = self.scale;
        let width = self.text_width();
        // SAFETY: контекст экрана окна для измерения; возвращается ниже.
        let dc = unsafe { GetDC(hwnd) };
        let measure = |f: HFONT, text: &str| {
            let mut r = rect(0, 0, width, 0);
            draw_text(dc, f, 0, 0, text, &mut r, WRAP | DT_CALCRECT);
            r.bottom - r.top
        };
        let label_h = measure(fonts.label, "ВЫ");
        let question_h = measure(fonts.question, self.view.question.trim());
        let answer_h = measure(fonts.answer, &self.answer_text());
        // SAFETY: dc получен GetDC для этого окна.
        unsafe {
            ReleaseDC(hwnd, dc);
        }
        let block = |h: i32| label_h + s.px(4) + h;
        self.layout = Layout {
            question_h,
            answer_h,
            content_h: s.px(16) + block(question_h) + s.px(16) + block(answer_h) + s.px(18),
        };
        (s.px(HEADER) + self.layout.content_h).clamp(s.px(MIN_HEIGHT), s.px(MAX_HEIGHT))
    }

    /// Подогнать высоту окна под содержимое; низ окна остаётся на месте.
    fn fit(&mut self, hwnd: HWND, place_in_corner: bool) {
        let height = self.relayout(hwnd);
        let width = self.scale.px(WIDTH);
        let mut wr = rect(0, 0, 0, 0);
        // SAFETY: hwnd — окно этого потока; структуры валидны для записи.
        unsafe {
            if place_in_corner {
                let mut area = rect(0, 0, 1280, 720);
                SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut area).cast(), 0);
                let margin = self.scale.px(MARGIN);
                wr = rect(
                    area.right - width - margin,
                    0,
                    area.right - margin,
                    area.bottom - margin,
                );
            } else {
                GetWindowRect(hwnd, &mut wr);
            }
            if place_in_corner || wr.bottom - wr.top != height || wr.right - wr.left != width {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    wr.left,
                    wr.bottom - height,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        self.clamp_scroll(hwnd);
    }

    fn max_scroll(&self, hwnd: HWND) -> i32 {
        let mut cr = rect(0, 0, 0, 0);
        // SAFETY: hwnd — окно этого потока.
        unsafe {
            GetClientRect(hwnd, &mut cr);
        }
        let visible = cr.bottom - self.scale.px(HEADER);
        (self.layout.content_h - visible).max(0)
    }

    fn clamp_scroll(&mut self, hwnd: HWND) {
        let max = self.max_scroll(hwnd);
        if self.follow {
            self.scroll = max;
        }
        self.scroll = self.scroll.clamp(0, max);
    }

    /// Крестик в правом верхнем углу (клиентские координаты).
    fn close_rect(&self, client_w: i32) -> RECT {
        let s = self.scale;
        let size = s.px(CLOSE);
        let top = (s.px(HEADER) - size) / 2;
        let right = client_w - s.px(8);
        rect(right - size, top, right, top + size)
    }

    fn sync_animation(&mut self, hwnd: HWND) {
        let want = self.view.status.animated();
        if want == self.animating {
            return;
        }
        // SAFETY: таймер этого окна; повторный SetTimer с тем же id заменяет.
        unsafe {
            if want {
                SetTimer(hwnd, ANIMATION_TIMER, ANIMATION_MS, None);
            } else {
                KillTimer(hwnd, ANIMATION_TIMER);
            }
        }
        self.animating = want;
    }

    fn paint(&self, dc: HDC, w: i32, h: i32) {
        let Some(fonts) = &self.fonts else {
            return;
        };
        let s = self.scale;
        let header = s.px(HEADER);
        let pad = s.px(PAD);
        fill(dc, rect(0, 0, w, h), BG);

        // --- заголовок ---
        fill(dc, rect(0, 0, w, header), HEADER_BG);
        let mut x = pad - s.px(4);
        if let Some(logo) = &self.logo {
            draw_logo(dc, logo, x, (header - logo.size) / 2);
            x += logo.size + s.px(10);
        }
        let mut r = rect(x, 0, w, header);
        draw_text(
            dc,
            fonts.title,
            ACCENT_HI,
            s.px(3),
            "J.A.R.V.I.S.",
            &mut r,
            DT_SINGLELINE | DT_VCENTER,
        );
        let close = self.close_rect(w);
        let status_color = if self.view.status == AnswerStatus::Failed {
            FAIL
        } else {
            MUTED
        };
        // DT_RIGHT не учитывает разрядку букв — текст уезжал под крестик.
        // Ширину считаем сами: размер без разрядки + разрядка на каждую букву.
        let label = self.view.status.label();
        let spacing = s.px(2);
        let mut m = rect(0, 0, w, header);
        draw_text(
            dc,
            fonts.label,
            0,
            0,
            label,
            &mut m,
            DT_SINGLELINE | DT_CALCRECT,
        );
        let label_w = m.right - m.left + spacing * label.chars().count() as i32;
        let right = close.left - s.px(4);
        let mut r = rect(right - label_w, 0, right, header);
        draw_text(
            dc,
            fonts.label,
            status_color,
            spacing,
            label,
            &mut r,
            DT_SINGLELINE | DT_VCENTER,
        );
        draw_cross(dc, close, s.px(5), s.px(1).max(1), MUTED);

        // Линия под заголовком: акцент слева; пока идёт ответ — бегущий отрезок.
        let thick = s.px(2).max(1);
        fill(dc, rect(0, header - 1, w, header), LINE);
        if self.animating {
            let seg = s.px(110);
            let t = self.started.elapsed().as_secs_f32() % 1.6 / 1.6;
            let left = (t * (w + seg) as f32) as i32 - seg;
            fill(
                dc,
                rect(left.max(0), header - thick, (left + seg).min(w), header),
                ACCENT,
            );
        } else {
            let color = if self.view.status == AnswerStatus::Failed {
                FAIL
            } else {
                ACCENT
            };
            fill(dc, rect(0, header - thick, s.px(72), header), color);
        }

        // --- содержимое (с прокруткой, обрезано под заголовком) ---
        // SAFETY: dc — буфер рисования; сохранённое состояние восстанавливается ниже.
        let saved = unsafe {
            let saved = SaveDC(dc);
            IntersectClipRect(dc, 0, header, w, h);
            saved
        };
        let text_left = pad + s.px(BAR_GAP);
        let text_right = text_left + self.text_width();
        let bar = s.px(2).max(1);
        let mut y = header + s.px(16) - self.scroll;
        let mut block = |label: &str,
                         label_color: u32,
                         bar_color: u32,
                         f: HFONT,
                         color: u32,
                         text: &str,
                         height: i32| {
            let mut r = rect(text_left, y, text_right, y + s.px(20));
            let label_h = draw_text(
                dc,
                fonts.label,
                label_color,
                s.px(2),
                label,
                &mut r,
                DT_SINGLELINE,
            );
            let top = y;
            y += label_h + s.px(4);
            let mut r = rect(text_left, y, text_right, y + height);
            draw_text(dc, f, color, 0, text, &mut r, WRAP);
            y += height;
            fill(dc, rect(pad, top + s.px(2), pad + bar, y), bar_color);
            y += s.px(16);
        };
        block(
            "ВЫ",
            MUTED,
            DIM,
            fonts.question,
            QUESTION,
            self.view.question.trim(),
            self.layout.question_h,
        );
        let (answer_color, bar_color, label_color) = match self.view.status {
            AnswerStatus::Failed => (TEXT, FAIL, FAIL),
            AnswerStatus::Thinking if self.view.answer.trim().is_empty() => (MUTED, ACCENT, ACCENT),
            _ => (TEXT, ACCENT, ACCENT),
        };
        block(
            "ДЖАРВИС",
            label_color,
            bar_color,
            fonts.answer,
            answer_color,
            &self.answer_text(),
            self.layout.answer_h,
        );
        // SAFETY: возвращает состояние, сохранённое выше.
        unsafe {
            RestoreDC(dc, saved);
        }

        // Полоса прокрутки — только если ответ не влез.
        let visible = h - header;
        if self.layout.content_h > visible && self.layout.content_h > 0 {
            let track = visible - s.px(12);
            let thumb = (track * visible / self.layout.content_h).max(s.px(24));
            let max = (self.layout.content_h - visible).max(1);
            let top = header + s.px(6) + (track - thumb) * self.scroll / max;
            let x = w - s.px(5);
            fill(dc, rect(x, top, x + s.px(2).max(1), top + thumb), DIM);
        }
    }
}

fn draw_cross(dc: HDC, r: RECT, half: i32, thick: i32, rgb: u32) {
    // Крестик из квадратиков по диагоналям — без пера, ровно при любом масштабе.
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    for i in -half..=half {
        fill(
            dc,
            rect(cx + i, cy + i, cx + i + thick, cy + i + thick),
            rgb,
        );
        fill(
            dc,
            rect(cx + i, cy - i, cx + i + thick, cy - i + thick),
            rgb,
        );
    }
}

/// Картинка реактора с предумноженной прозрачностью (для AlphaBlend).
fn make_logo(paint: LogoPainter, size: i32) -> Option<Logo> {
    let side = u32::try_from(size).ok().filter(|s| *s > 0)?;
    let rgba = paint(side);
    if rgba.len() != (side * side * 4) as usize {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            // Отрицательная высота — строки сверху вниз, как в RGBA.
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [Default::default()],
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: info описывает 32-битную картинку size × size; Windows выделяет
    // память под пиксели и отдаёт указатель в `bits`.
    let bitmap = unsafe {
        CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if bitmap.is_null() || bits.is_null() {
        delete_object(bitmap);
        return None;
    }
    // SAFETY: `bits` — пиксели только что созданной картинки, ровно
    // side × side × 4 байт, принадлежат ей до DeleteObject.
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len()) };
    for (dst, src) in pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(rgba.as_chunks::<4>().0)
    {
        let a = u16::from(src[3]);
        let pre = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
        dst.copy_from_slice(&[pre(src[2]), pre(src[1]), pre(src[0]), src[3]]);
    }
    Some(Logo { bitmap, size })
}

fn draw_logo(dc: HDC, logo: &Logo, x: i32, y: i32) {
    // SAFETY: картинка выбирается во временный контекст, рисуется с
    // прозрачностью и возвращается; контекст удаляется.
    unsafe {
        let src = CreateCompatibleDC(dc);
        let old = SelectObject(src, logo.bitmap);
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        AlphaBlend(
            dc, x, y, logo.size, logo.size, src, 0, 0, logo.size, logo.size, blend,
        );
        SelectObject(src, old);
        DeleteDC(src);
    }
}

/// Точка курсора в клиентских координатах.
fn cursor_in_client(hwnd: HWND) -> POINT {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: p — валидная структура; hwnd — окно этого потока.
    unsafe {
        GetCursorPos(&mut p);
        ScreenToClient(hwnd, &mut p);
    }
    p
}

fn client_width(hwnd: HWND) -> i32 {
    let mut cr = rect(0, 0, 0, 0);
    // SAFETY: hwnd — окно этого потока.
    unsafe {
        GetClientRect(hwnd, &mut cr);
    }
    cr.right
}

fn inside(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: стандартная пара BeginPaint/EndPaint; рисуем в буфер того же
    // размера и копируем его разом — без мерцания; буфер освобождается.
    unsafe {
        let dc = BeginPaint(hwnd, &mut ps);
        let mut cr = rect(0, 0, 0, 0);
        GetClientRect(hwnd, &mut cr);
        let (w, h) = (cr.right, cr.bottom);
        let mem = CreateCompatibleDC(dc);
        let bitmap = CreateCompatibleBitmap(dc, w.max(1), h.max(1));
        let old = SelectObject(mem, bitmap);
        if let Some(st) = state(hwnd) {
            st.paint(mem, w, h);
        }
        BitBlt(
            dc,
            0,
            0,
            w,
            h,
            mem,
            0,
            0,
            windows_sys::Win32::Graphics::Gdi::SRCCOPY,
        );
        SelectObject(mem, old);
        DeleteObject(bitmap);
        DeleteDC(mem);
        EndPaint(hwnd, &ps);
    }
}

fn invalidate(hwnd: HWND) {
    // SAFETY: перерисовать всё окно этого потока (фон не стирать — рисуем сами).
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: для WM_NCCREATE lParam — указатель на CREATESTRUCTW,
            // lpCreateParams — наш Box<WindowState> из run_window (забираем
            // ровно один раз, освобождаем в WM_NCDESTROY).
            unsafe {
                let cs = &*(l as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        WM_CREATE => {
            let Some(st) = state(hwnd) else {
                return -1;
            };
            // SAFETY: hwnd — создаваемое окно этого потока.
            let dpi = unsafe { GetDpiForWindow(hwnd) };
            st.apply_dpi(dpi);
            st.fit(hwnd, true);
            st.sync_animation(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            on_paint(hwnd);
            0
        }
        WM_CONTENT_CHANGED => {
            if let Some(st) = state(hwnd) {
                let view = st
                    .shared
                    .view
                    .lock()
                    .map(|v| v.clone())
                    .unwrap_or_else(|e| e.into_inner().clone());
                if view != st.view {
                    st.view = view;
                    st.fit(hwnd, false);
                    st.sync_animation(hwnd);
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_TIMER => {
            if let Some(st) = state(hwnd) {
                // Точки «Обрабатываю запрос…» меняют высоту редко, но могут.
                if st.view.status == AnswerStatus::Thinking {
                    st.fit(hwnd, false);
                }
                invalidate(hwnd);
            }
            0
        }
        WM_DPICHANGED => {
            if let Some(st) = state(hwnd) {
                st.apply_dpi((w & 0xFFFF) as u32);
                // SAFETY: для WM_DPICHANGED lParam — RECT с предложенным местом.
                let suggested = unsafe { &*(l as *const RECT) };
                // SAFETY: окно этого потока; ставим на предложенное место,
                // высоту подгоняет fit.
                unsafe {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                st.fit(hwnd, false);
                invalidate(hwnd);
            }
            0
        }
        // Щелчок не делает окно активным — фокус остаётся, где был.
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_NCHITTEST => {
            // SAFETY: стандартная проверка; дальше уточняем для заголовка.
            let hit = unsafe { DefWindowProcW(hwnd, msg, w, l) };
            if hit != HTCLIENT as LRESULT {
                return hit;
            }
            match state(hwnd) {
                Some(st) => {
                    let p = cursor_in_client(hwnd);
                    let close = st.close_rect(client_width(hwnd));
                    if p.y < st.scale.px(HEADER) && !inside(&close, p) {
                        HTCAPTION as LRESULT
                    } else {
                        hit
                    }
                }
                None => hit,
            }
        }
        WM_SETCURSOR => {
            if let Some(st) = state(hwnd)
                && inside(&st.close_rect(client_width(hwnd)), cursor_in_client(hwnd))
            {
                // SAFETY: системный курсор-рука, освобождать не нужно.
                unsafe {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                }
                return 1;
            }
            // SAFETY: остальное — стандартная обработка.
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_LBUTTONUP => {
            if let Some(st) = state(hwnd) {
                let p = POINT {
                    x: (l & 0xFFFF) as i16 as i32,
                    y: ((l >> 16) & 0xFFFF) as i16 as i32,
                };
                if inside(&st.close_rect(client_width(hwnd)), p) {
                    // SAFETY: закрываем своё окно в его потоке.
                    unsafe {
                        DestroyWindow(hwnd);
                    }
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            if let Some(st) = state(hwnd) {
                let delta = ((w >> 16) & 0xFFFF) as i16 as i32;
                let max = st.max_scroll(hwnd);
                st.scroll = (st.scroll - delta * st.scale.px(WHEEL_STEP) / 120).clamp(0, max);
                st.follow = st.scroll >= max;
                invalidate(hwnd);
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
            // SAFETY: таймер этого окна; завершить цикл сообщений потока.
            unsafe {
                KillTimer(hwnd, ANIMATION_TIMER);
                PostQuitMessage(0);
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
                    st.shared.hwnd.store(0, Ordering::Release);
                }
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        // SAFETY: остальное — стандартная обработка.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_swaps_red_and_blue() {
        assert_eq!(colorref(0x1E8CF0), 0xF08C1E);
        assert_eq!(colorref(0x000000), 0);
    }

    #[test]
    fn scale_rounds_down_proportionally() {
        assert_eq!(Scale(96).px(440), 440);
        assert_eq!(Scale(144).px(440), 660);
        assert_eq!(Scale(120).px(15), 18);
    }

    #[test]
    fn only_live_answer_is_animated() {
        assert!(AnswerStatus::Thinking.animated());
        assert!(AnswerStatus::Answering.animated());
        assert!(!AnswerStatus::Done.animated());
        assert!(!AnswerStatus::Failed.animated());
    }
}
