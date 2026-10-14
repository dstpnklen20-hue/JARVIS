//! Заметка Google Keep в Firefox: дописать строку в конец её текста.
//!
//! Firefox отдаёт страницы через UI Automation, но поля Keep — без имён, и
//! программно поставить курсор в текст заметки нельзя (фокус уходит на всю
//! страницу). Поэтому как человек: найти открытую заметку (документ с её
//! адресом → поле заголовка → следующее за ним поле — текст), вывести Firefox
//! вперёд, щёлкнуть в текст и только если фокус действительно там — нажать
//! Ctrl+End, Enter и напечатать строку. Любая проверка не прошла — ничего не
//! нажимается (иначе ввод ушёл бы в чужое окно).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId,
    UIA_EditControlTypeId, UIA_IsKeyboardFocusablePropertyId, UIA_ValuePatternId,
};
use windows_sys::Win32::Foundation::{HWND as RawHwnd, POINT as RawPoint};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
    SendInput, VK_CONTROL, VK_END, VK_RETURN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

use super::actions::open_url;
use super::browser::{activate_browser_tab, bring_to_front, browser_windows, is_foreground};

/// Сколько ждать, пока заметка откроется (страница Keep грузится секунды).
const NOTE_WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(400);

/// Открыть заметку в Firefox (если ещё не открыта) и вывести её вперёд.
/// Пока человек диктует, страница успеет загрузиться.
pub fn keep_open_note(url: &str, title: &str) -> Result<(), String> {
    super::use_per_monitor_dpi();
    with_automation(|automation| ensure_note(automation, url, title).map(|_| ()))
}

/// Дописать строку `line` в конец заметки (открыв её, если нужно).
pub fn keep_append_line(url: &str, title: &str, line: &str) -> Result<(), String> {
    super::use_per_monitor_dpi();
    with_automation(|automation| {
        let note = ensure_note(automation, url, title)?;
        if !bring_to_front(note.window) || !is_foreground(note.window) {
            return Err("Firefox не стал активным окном — ничего не печатаю".into());
        }
        click(note.point);
        std::thread::sleep(Duration::from_millis(250));
        // Главная проверка: печатаем, только если фокус — в тексте этой заметки.
        let focused_ok = is_foreground(note.window)
            // SAFETY: обычный вызов UI Automation; элемент освобождается сам.
            && unsafe { automation.GetFocusedElement() }
                .ok()
                .is_some_and(|f| class_name(&f) == note.class);
        if !focused_ok {
            return Err("фокус не в тексте заметки — ничего не печатаю".into());
        }
        press_vk(VK_CONTROL, false, false);
        press_vk(VK_END, false, true);
        press_vk(VK_END, true, true);
        press_vk(VK_CONTROL, true, false);
        press_vk(VK_RETURN, false, false);
        press_vk(VK_RETURN, true, false);
        type_text(line);
        Ok(())
    })
}

/// Открытая заметка: окно Firefox, класс поля текста, куда щёлкнуть.
struct OpenNote {
    window: RawHwnd,
    class: String,
    point: RawPoint,
}

fn with_automation<T>(work: impl FnOnce(&IUIAutomation) -> Result<T, String>) -> Result<T, String> {
    // SAFETY: COM инициализируется в потоке действия; S_FALSE/RPC_E_CHANGED_MODE —
    // уже инициализирован, тоже годится (как в activate_browser_tab).
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // SAFETY: создание объекта UI Automation; освобождается при выходе из области.
    let result =
        unsafe { CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("UI Automation: {}", e.message()))
            .and_then(|automation| work(&automation));
    if init.is_ok() {
        // SAFETY: парный вызов к успешному CoInitializeEx в этом потоке (все
        // COM-объекты к этому времени освобождены).
        unsafe { CoUninitialize() };
    }
    result
}

/// Дождаться, пока заметка будет открыта и видна; при необходимости
/// переключиться на вкладку Keep или открыть адрес заметки.
fn ensure_note(automation: &IUIAutomation, url: &str, title: &str) -> Result<OpenNote, String> {
    let started = Instant::now();
    let mut tried_tab = false;
    let mut opened = false;
    loop {
        if let Some((window, body)) = find_body(automation, url, title) {
            bring_to_front(window);
            std::thread::sleep(Duration::from_millis(200));
            if let Some(note) = visible(automation, window, &body) {
                return Ok(note);
            }
            // Заметка есть, но не на экране — может, в другой вкладке Keep.
            if !tried_tab {
                tried_tab = true;
                let _ = activate_browser_tab(&|t| t.starts_with("Google Keep"));
                continue;
            }
        }
        if !opened && (tried_tab || started.elapsed() > Duration::from_secs(1)) {
            opened = true;
            open_url(url).map_err(|e| format!("не удалось открыть заметку: {e}"))?;
        }
        if started.elapsed() > NOTE_WAIT {
            return Err("заметка не открылась (нет входа в Google в Firefox?)".into());
        }
        if !tried_tab && !opened {
            tried_tab = true;
            let _ = activate_browser_tab(&|t| t.starts_with("Google Keep"));
        }
        std::thread::sleep(POLL);
    }
}

/// Поле текста заметки: в документе с адресом заметки — поле заголовка
/// (`title`, в открытом редакторе оно фокусируемое), за ним — текст.
fn find_body(
    automation: &IUIAutomation,
    url: &str,
    title: &str,
) -> Option<(RawHwnd, IUIAutomationElement)> {
    // SAFETY: условия и поиск UI Automation; интерфейсы освобождаются сами,
    // окна — из EnumWindows (browser_windows).
    unsafe {
        let doc = automation
            .CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_DocumentControlTypeId.0),
            )
            .ok()?;
        let edit = automation
            .CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_EditControlTypeId.0),
            )
            .ok()?;
        let focusable = automation
            .CreatePropertyCondition(UIA_IsKeyboardFocusablePropertyId, &VARIANT::from(true))
            .ok()?;
        let title_field = automation.CreateAndCondition(&edit, &focusable).ok()?;
        let walker = automation.RawViewWalker().ok()?;
        for window in browser_windows() {
            let Ok(root) = automation.ElementFromHandle(HWND(window)) else {
                continue;
            };
            let Ok(docs) = root.FindAll(TreeScope_Descendants, &doc) else {
                continue;
            };
            for i in 0..docs.Length().unwrap_or(0) {
                let Ok(d) = docs.GetElement(i) else { continue };
                if value(&d).as_deref() != Some(url) {
                    continue;
                }
                let Ok(fields) = d.FindAll(TreeScope_Descendants, &title_field) else {
                    continue;
                };
                for j in 0..fields.Length().unwrap_or(0) {
                    let Ok(field) = fields.GetElement(j) else {
                        continue;
                    };
                    if value(&field).as_deref().map(str::trim) == Some(title.trim())
                        && let Ok(body) = walker.GetNextSiblingElement(&field)
                    {
                        return Some((window, body));
                    }
                }
            }
        }
        None
    }
}

/// Текст заметки виден на экране: элемент под точкой щелчка — он сам или
/// его часть. Возвращает, куда щёлкнуть (у нижнего края — в последние строки).
fn visible(
    automation: &IUIAutomation,
    window: RawHwnd,
    body: &IUIAutomationElement,
) -> Option<OpenNote> {
    // SAFETY: чтение свойств элементов UI Automation; интерфейсы освобождаются сами.
    unsafe {
        let rect = body.CurrentBoundingRectangle().ok()?;
        if rect.right - rect.left < 40 || rect.bottom - rect.top < 20 {
            return None;
        }
        let point = POINT {
            x: rect.left + 30,
            y: rect.bottom - 8,
        };
        let class = class_name(body);
        let walker = automation.RawViewWalker().ok()?;
        let mut hit = automation.ElementFromPoint(point).ok()?;
        for _ in 0..6 {
            if class_name(&hit) == class {
                return Some(OpenNote {
                    window,
                    class,
                    point: RawPoint {
                        x: point.x,
                        y: point.y,
                    },
                });
            }
            hit = walker.GetParentElement(&hit).ok()?;
        }
        None
    }
}

fn value(element: &IUIAutomationElement) -> Option<String> {
    // SAFETY: чтение значения через ValuePattern; интерфейсы освобождаются сами.
    unsafe {
        element
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()?
            .CurrentValue()
            .ok()
            .map(|v| v.to_string())
    }
}

fn class_name(element: &IUIAutomationElement) -> String {
    // SAFETY: чтение свойства элемента UI Automation.
    unsafe { element.CurrentClassName() }
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// Щелчок левой кнопкой в точке; курсор возвращается на место.
fn click(point: RawPoint) {
    let mut old = RawPoint { x: 0, y: 0 };
    let mouse = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [mouse(MOUSEEVENTF_LEFTDOWN), mouse(MOUSEEVENTF_LEFTUP)];
    // SAFETY: `old` — валидная структура; массив INPUT из двух элементов,
    // размер структуры передан точно.
    unsafe {
        GetCursorPos(&mut old);
        SetCursorPos(point.x, point.y);
        SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
        std::thread::sleep(Duration::from_millis(60));
        SetCursorPos(old.x, old.y);
    }
}

fn key(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    // SAFETY: срез INPUT, длина и размер структуры переданы точно.
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        );
    }
}

fn press_vk(vk: u16, up: bool, extended: bool) {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    send(&[key(vk, 0, flags)]);
    std::thread::sleep(Duration::from_millis(20));
}

/// Напечатать текст символами Юникода (не трогая буфер обмена и раскладку).
fn type_text(text: &str) {
    for unit in text.encode_utf16() {
        send(&[
            key(0, unit, KEYEVENTF_UNICODE),
            key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ]);
        std::thread::sleep(Duration::from_millis(4));
    }
}
