//! Нажатие клавиш через SendInput со скан-кодами — так нажатия доходят и до
//! игр (DirectInput/Raw Input читают скан-коды, а не виртуальные коды).
//!
//! Отпускание гарантировано: зажатые клавиши запоминаются, и отпускаются
//! даже при панике (Drop) и аварийной остановке ([`release_all_keys`]).

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, SendInput,
};

/// Клавиши, которые сейчас зажаты Джарвисом.
static PRESSED: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

/// Клавиши с префиксом E0 («расширенные»). MapVirtualKey для части из них
/// префикс не возвращает (стрелки делят скан-код с цифровым блоком), а без
/// флага игра получит «8 на цифровом блоке» вместо «стрелки вверх».
const EXTENDED_VKS: &[u16] = &[
    0x21, 0x22, 0x23, 0x24, // PageUp, PageDown, End, Home
    0x25, 0x26, 0x27, 0x28, // стрелки
    0x2C, 0x2D, 0x2E, // PrintScreen, Insert, Delete
    0x5B, 0x5C, 0x5D, // Win, Win, Menu
    0x6F, 0x90, // Num /, NumLock
    0xA3, 0xA5, // правые Ctrl и Alt
];

/// Скан-код и признак «расширенной» клавиши для текущей раскладки.
fn scan_code(vk: u16) -> (u16, bool) {
    // SAFETY: чистая функция преобразования кодов, без указателей.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
    let extended = matches!(scan & 0xFF00, 0xE000 | 0xE100) || EXTENDED_VKS.contains(&vk);
    ((scan & 0xFF) as u16, extended)
}

fn key_input(vk: u16, up: bool) -> INPUT {
    let (scan, extended) = scan_code(vk);
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    let count = u32::try_from(inputs.len()).map_err(|e| e.to_string())?;
    // SAFETY: `inputs` — валидный массив INPUT длины `count`; размер структуры
    // передан точно.
    let sent = unsafe { SendInput(count, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    if sent == count {
        Ok(())
    } else {
        Err(format!(
            "SendInput отправил {sent} из {count} событий: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// Отпускает переданные клавиши при выходе из области видимости.
struct Release<'a>(&'a [u16]);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let ups: Vec<INPUT> = self.0.iter().rev().map(|vk| key_input(*vk, true)).collect();
        let _ = send(&ups);
        if let Ok(mut pressed) = PRESSED.lock() {
            for vk in self.0 {
                pressed.remove(vk);
            }
        }
    }
}

/// Нажимает клавиши по порядку, держит `hold`, отпускает в обратном порядке.
pub fn press_keys(vks: &[u16], hold: Duration) -> Result<(), String> {
    if let Ok(mut pressed) = PRESSED.lock() {
        pressed.extend(vks.iter().copied());
    }
    let release = Release(vks);
    let downs: Vec<INPUT> = vks.iter().map(|vk| key_input(*vk, false)).collect();
    let result = send(&downs);
    if result.is_ok() && !hold.is_zero() {
        std::thread::sleep(hold);
    }
    drop(release);
    result
}

/// Аварийная остановка: отпустить всё, что зажато.
pub fn release_all_keys() {
    let keys: Vec<u16> = PRESSED
        .lock()
        .map(|mut p| std::mem::take(&mut *p).into_iter().collect())
        .unwrap_or_default();
    let ups: Vec<INPUT> = keys.iter().rev().map(|vk| key_input(*vk, true)).collect();
    let _ = send(&ups);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_codes_for_common_keys() {
        // Скан-коды набора 1 не зависят от раскладки для этих клавиш.
        assert_eq!(scan_code(0x1B), (0x01, false)); // Esc
        assert_eq!(scan_code(0x20), (0x39, false)); // Space
        assert_eq!(scan_code(0x41), (0x1E, false)); // A
        assert_eq!(scan_code(0x26), (0x48, true)); // стрелка вверх — расширенная
        assert_eq!(scan_code(0x68), (0x48, false)); // 8 на цифровом блоке — нет
        assert!(scan_code(0xA3).1, "правый Ctrl — расширенная");
    }
}
