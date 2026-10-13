//! Вкладки браузера и процессы: найти открытую вкладку Firefox по заголовку
//! (UI Automation), выбрать её и вывести окно вперёд; проверить, запущен ли
//! процесс.
//!
//! Firefox показывает свои вкладки в дереве UI Automation как элементы
//! TabItem внутри полосы `tabbrowser-tabs`; вкладки внутри страниц (YouTube
//! Music, Google Keep) лежат глубже и в эту полосу не входят. Вкладки
//! свёрнутой группы в дереве не видны: группу (кнопка `tab-group-label`)
//! приходится раскрыть — нашлась вкладка — группа остаётся открытой, нет —
//! сворачивается обратно.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, ExpandCollapseState_Collapsed, IUIAutomation, IUIAutomationCondition,
    IUIAutomationElement, IUIAutomationExpandCollapsePattern, IUIAutomationSelectionItemPattern,
    TreeScope_Children, TreeScope_Descendants, UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId,
    UIA_ControlTypePropertyId, UIA_ExpandCollapsePatternId, UIA_SelectionItemPatternId,
    UIA_TabItemControlTypeId,
};
use windows_sys::Win32::Foundation::{HWND as RawHwnd, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows_sys::core::BOOL;

use super::{OwnedHandle, last_error};

const FIREFOX_WINDOW_CLASS: &str = "MozillaWindowClass";
const FIREFOX_TAB_STRIP: &str = "tabbrowser-tabs";
const FIREFOX_TAB_GROUP: &str = "tab-group-label";
/// Сколько ждать, пока вкладки раскрытой группы появятся в дереве (обычно ~60 мс).
const GROUP_EXPAND_WAIT: std::time::Duration = std::time::Duration::from_millis(600);
/// Виртуальная клавиша без назначения (0xE8 — «unassigned» в документации
/// Windows): её «нажатие» ни на что не влияет, но снимает запрет на смену фокуса.
const VK_UNASSIGNED: u16 = 0xE8;

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
        let group = automation
            .CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(FIREFOX_TAB_GROUP))?;
        for &hwnd in windows {
            // Окно закрылось или полосы вкладок нет (служебное окно) — дальше.
            let Ok(window) = automation.ElementFromHandle(HWND(hwnd)) else {
                continue;
            };
            let Ok(tabs_strip) = window.FindFirst(TreeScope_Descendants, &strip) else {
                continue;
            };
            // Сначала видимые вкладки.
            let (found, seen) = find_tab(&tabs_strip, &tab, matches)?;
            if let Some(title) = found {
                return Ok(Some((hwnd, title)));
            }
            // Затем свёрнутые группы — по одной. Окно — сначала вперёд: пока
            // оно закрыто другими, Firefox не дорисовывает раскрытие группы и
            // вкладки в дереве не появляются (проверено: 0 новых за 1 с против
            // всех за 0.1 с у окна на виду).
            let groups = tabs_strip.FindAll(TreeScope_Children, &group)?;
            if groups.Length()? > 0 {
                bring_to_front(hwnd);
            }
            for i in 0..groups.Length()? {
                let label = groups.GetElement(i)?;
                let Ok(toggle) = label.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                    UIA_ExpandCollapsePatternId,
                ) else {
                    continue;
                };
                if toggle.CurrentExpandCollapseState()? != ExpandCollapseState_Collapsed {
                    continue;
                }
                toggle.Expand()?;
                let deadline = std::time::Instant::now() + GROUP_EXPAND_WAIT;
                let found = loop {
                    let (hit, count) = find_tab(&tabs_strip, &tab, matches)?;
                    if hit.is_some() || count > seen || std::time::Instant::now() >= deadline {
                        break hit;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                };
                if let Some(title) = found {
                    return Ok(Some((hwnd, title)));
                }
                // Не здесь — свернуть, как было.
                let _ = toggle.Collapse();
            }
        }
        Ok(None)
    }
}

/// Ищет среди видимых вкладок полосы подходящую и выбирает её.
/// Возвращает (заголовок выбранной, сколько вкладок видно).
fn find_tab(
    strip: &IUIAutomationElement,
    tab: &IUIAutomationCondition,
    matches: &dyn Fn(&str) -> bool,
) -> windows::core::Result<(Option<String>, i32)> {
    // SAFETY: элементы — из дерева UI Automation, интерфейсы освобождаются Drop.
    unsafe {
        let tabs = strip.FindAll(TreeScope_Children, tab)?;
        let count = tabs.Length()?;
        for i in 0..count {
            let item = tabs.GetElement(i)?;
            let title = item.CurrentName()?.to_string();
            if matches(&title) {
                let select: IUIAutomationSelectionItemPattern =
                    item.GetCurrentPatternAs(UIA_SelectionItemPatternId)?;
                select.Select()?;
                return Ok((Some(title), count));
            }
        }
        Ok((None, count))
    }
}

/// Вывести вперёд верхнее окно Firefox (после открытия новой вкладки).
/// `false` — браузер не открыт или Windows не дала сменить окно.
pub fn focus_browser() -> bool {
    browser_windows()
        .first()
        .is_some_and(|&hwnd| bring_to_front(hwnd))
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

/// Развернуть свёрнутое окно и сделать активным; `true` — окно стало активным.
/// Windows не даёт фоновому процессу забирать фокус, поэтому по очереди:
/// просто попросить; подключиться к вводу активного окна (AttachThreadInput);
/// «нажать» клавишу без назначения — после своего ввода процессу разрешено
/// сменить активное окно.
fn bring_to_front(hwnd: RawHwnd) -> bool {
    // SAFETY: hwnd — окно из EnumWindows; если оно уже закрыто, функции просто вернут ошибку.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    if is_foreground(hwnd) {
        return true;
    }
    // SAFETY: активное окно и его поток — от WinAPI; второй аргумент
    // GetWindowThreadProcessId (номер процесса) не нужен — null.
    let foreground_thread =
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()) };
    // SAFETY: без аргументов.
    let me = unsafe { GetCurrentThreadId() };
    if foreground_thread != 0 && foreground_thread != me {
        // SAFETY: подключение ввода снимается сразу после вызова.
        unsafe {
            if AttachThreadInput(me, foreground_thread, 1) != 0 {
                BringWindowToTop(hwnd);
                SetForegroundWindow(hwnd);
                AttachThreadInput(me, foreground_thread, 0);
            }
        }
    }
    if is_foreground(hwnd) {
        return true;
    }
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_UNASSIGNED,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(false), key(true)];
    // SAFETY: массив INPUT из двух элементов, размер структуры передан точно.
    unsafe {
        SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
        SetForegroundWindow(hwnd);
    }
    is_foreground(hwnd)
}

fn is_foreground(hwnd: RawHwnd) -> bool {
    // SAFETY: без аргументов.
    unsafe { GetForegroundWindow() == hwnd }
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

    /// Живая проверка на открытом Firefox: вкладка из переменной окружения
    /// JARVIS_TAB_TEST (часть заголовка) выбирается, даже если лежит в
    /// свёрнутой группе, и окно выходит вперёд. Запуск вручную:
    /// `JARVIS_TAB_TEST=Календарь cargo test -p jarvis-platform live_tab -- --ignored`.
    #[test]
    #[ignore = "переключает вкладку в настоящем Firefox"]
    fn live_tab_activation() {
        let wanted = std::env::var("JARVIS_TAB_TEST").unwrap();
        let started = std::time::Instant::now();
        let found = activate_browser_tab(&|title| title.contains(&wanted)).unwrap();
        eprintln!("найдено: {found:?} за {} мс", started.elapsed().as_millis());
        assert!(found.is_some());
        let front = browser_windows().first().copied().unwrap();
        assert!(is_foreground(front), "окно браузера не вышло вперёд");
    }
}
