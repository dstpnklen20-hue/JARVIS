//! Вкладки браузера и процессы: найти открытую вкладку Firefox по заголовку
//! (UI Automation), выбрать её и вывести окно вперёд; проверить, запущен ли
//! процесс.
//!
//! Firefox показывает свои вкладки в дереве UI Automation как элементы
//! TabItem внутри полосы `tabbrowser-tabs`; вкладки внутри страниц (YouTube
//! Music, Google Keep) лежат глубже и в эту полосу не входят.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationSelectionItemPattern, TreeScope_Children,
    TreeScope_Descendants, UIA_AutomationIdPropertyId, UIA_ControlTypePropertyId,
    UIA_SelectionItemPatternId, UIA_TabItemControlTypeId,
};
use windows_sys::Win32::Foundation::{HWND as RawHwnd, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows_sys::core::BOOL;

use super::{OwnedHandle, last_error};

const FIREFOX_WINDOW_CLASS: &str = "MozillaWindowClass";
const FIREFOX_TAB_STRIP: &str = "tabbrowser-tabs";

/// Ищет вкладку Firefox, заголовок которой подходит, делает её текущей и
/// выводит окно вперёд. `Ok(None)` — браузер не открыт или такой вкладки нет.
pub fn activate_browser_tab(matches: &dyn Fn(&str) -> bool) -> Result<Option<String>, String> {
    let windows = browser_windows();
    if windows.is_empty() {
        return Ok(None);
    }
    // SAFETY: COM инициализируется в потоке действия; S_FALSE/RPC_E_CHANGED_MODE —
    // уже инициализирован, тоже годится (см. set_volume).
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // Все COM-объекты освобождаются внутри — до CoUninitialize.
    let found = select_tab(&windows, matches);
    if init.is_ok() {
        // SAFETY: парный вызов к успешному CoInitializeEx в этом потоке.
        unsafe { CoUninitialize() };
    }
    match found.map_err(|e| e.message().to_string())? {
        Some((hwnd, title)) => {
            bring_to_front(hwnd);
            Ok(Some(title))
        }
        None => Ok(None),
    }
}

fn select_tab(
    windows: &[RawHwnd],
    matches: &dyn Fn(&str) -> bool,
) -> windows::core::Result<Option<(RawHwnd, String)>> {
    // SAFETY: стандартная последовательность вызовов UI Automation; интерфейсы
    // освобождаются автоматически (Drop у windows-rs), окна — из EnumWindows.
    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let strip = automation.CreatePropertyCondition(
            UIA_AutomationIdPropertyId,
            &VARIANT::from(FIREFOX_TAB_STRIP),
        )?;
        let tab = automation.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_TabItemControlTypeId.0),
        )?;
        for &hwnd in windows {
            // Окно закрылось или полосы вкладок нет (служебное окно) — дальше.
            let Ok(window) = automation.ElementFromHandle(HWND(hwnd)) else {
                continue;
            };
            let Ok(tabs_strip) = window.FindFirst(TreeScope_Descendants, &strip) else {
                continue;
            };
            let tabs = tabs_strip.FindAll(TreeScope_Children, &tab)?;
            for i in 0..tabs.Length()? {
                let item = tabs.GetElement(i)?;
                let title = item.CurrentName()?.to_string();
                if matches(&title) {
                    let select: IUIAutomationSelectionItemPattern =
                        item.GetCurrentPatternAs(UIA_SelectionItemPatternId)?;
                    select.Select()?;
                    return Ok(Some((hwnd, title)));
                }
            }
        }
        Ok(None)
    }
}

/// Видимые окна Firefox сверху вниз (последнее активное — первым).
fn browser_windows() -> Vec<RawHwnd> {
    let mut found: Vec<RawHwnd> = Vec::new();
    // SAFETY: колбэк получает указатель на `found`, который живёт до конца
    // EnumWindows и после него не используется колбэком.
    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut found as *mut Vec<RawHwnd> as LPARAM,
        );
    }
    found
}

unsafe extern "system" fn collect_window(hwnd: RawHwnd, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` — указатель на Vec из `browser_windows`, живой на время
    // EnumWindows; других ссылок на него в это время нет.
    let found = unsafe { &mut *(lparam as *mut Vec<RawHwnd>) };
    let mut class = [0u16; 64];
    // SAFETY: буфер и его длина в символах переданы точно; hwnd — от EnumWindows.
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    let is_firefox =
        len > 0 && String::from_utf16_lossy(&class[..len as usize]) == FIREFOX_WINDOW_CLASS;
    // SAFETY: hwnd — от EnumWindows.
    if is_firefox && unsafe { IsWindowVisible(hwnd) } != 0 {
        found.push(hwnd);
    }
    1 // продолжать перебор
}

/// Развернуть свёрнутое окно и сделать активным. Windows не даёт фоновому
/// процессу забирать фокус — тогда на время вызова поток Джарвиса
/// подключается к вводу активного окна (штатный способ AttachThreadInput).
fn bring_to_front(hwnd: RawHwnd) {
    // SAFETY: hwnd — окно из EnumWindows; если оно уже закрыто, функции просто вернут ошибку.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd) != 0 {
            return;
        }
    }
    // SAFETY: активное окно и его поток — от WinAPI; второй аргумент
    // GetWindowThreadProcessId (номер процесса) не нужен — null.
    let foreground_thread =
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()) };
    // SAFETY: без аргументов.
    let me = unsafe { GetCurrentThreadId() };
    if foreground_thread == 0 || foreground_thread == me {
        return;
    }
    // SAFETY: подключение ввода снимается сразу после вызова.
    unsafe {
        if AttachThreadInput(me, foreground_thread, 1) != 0 {
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
            AttachThreadInput(me, foreground_thread, 0);
        }
    }
}

/// Запущен ли процесс с таким именем файла (`steam.exe`), без учёта регистра.
pub fn process_running(name: &str) -> Result<bool, String> {
    let wanted = name.trim().to_lowercase();
    // SAFETY: снимок списка процессов; дескриптор закрывает OwnedHandle.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(last_error("CreateToolhelp32Snapshot").to_string());
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: снимок открыт, у `entry` верный dwSize.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase() == wanted {
            return Ok(true);
        }
        // SAFETY: то же, что для Process32FirstW.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_found_and_unknown_is_not() {
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().to_uppercase();
        assert!(process_running(&name).unwrap(), "{name}");
        assert!(!process_running("нет-такой-программы-jarvis.exe").unwrap());
    }

    #[test]
    fn missing_tab_is_not_an_error() {
        // Браузер может быть открыт или нет — в обоих случаях не ошибка.
        let found = activate_browser_tab(&|_| false).unwrap();
        assert_eq!(found, None);
    }
}
