//! Глобальная горячая клавиша (аварийная остановка).
//!
//! RegisterHotKey без окна: нажатие приходит как WM_HOTKEY в очередь потока,
//! где клавиша зарегистрирована, — это главный поток с циклом сообщений.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};

use super::last_error;
use crate::PlatformError;

/// Зарегистрированная клавиша; снимается при удалении (в том же потоке).
pub struct HotkeyGuard {
    id: i32,
}

impl Drop for HotkeyGuard {
    fn drop(&mut self) {
        // SAFETY: снимаем ровно ту регистрацию, что сделали в register_hotkey.
        unsafe {
            UnregisterHotKey(std::ptr::null_mut(), self.id);
        }
    }
}

/// Регистрирует клавишу вида «Ctrl+Alt+F12» с номером `id`.
pub fn register_hotkey(id: i32, spec: &str) -> Result<HotkeyGuard, String> {
    let (mods, vk) = parse_hotkey(spec)?;
    // SAFETY: регистрация для текущего потока (окно null).
    let ok = unsafe { RegisterHotKey(std::ptr::null_mut(), id, mods | MOD_NOREPEAT, vk) };
    if ok == 0 {
        let err: PlatformError = last_error("RegisterHotKey");
        return Err(format!("«{spec}» уже занята другой программой ({err})"));
    }
    Ok(HotkeyGuard { id })
}

/// «Ctrl+Alt+F12» → модификаторы и виртуальный код клавиши.
pub fn parse_hotkey(spec: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let mut mods: HOT_KEY_MODIFIERS = 0;
    let mut key: Option<u32> = None;
    for part in spec.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            other => {
                if key.is_some() {
                    return Err(format!("в «{spec}» больше одной клавиши"));
                }
                key = Some(
                    vk_code(other)
                        .ok_or_else(|| format!("неизвестная клавиша «{part}» в «{spec}»"))?,
                );
            }
        }
    }
    let key = key.ok_or_else(|| format!("в «{spec}» нет основной клавиши"))?;
    if mods == 0 {
        return Err(format!(
            "«{spec}»: нужен хотя бы один модификатор (Ctrl, Alt, Shift, Win)"
        ));
    }
    Ok((mods, key))
}

fn vk_code(name: &str) -> Option<u32> {
    let code = match name {
        "pause" => 0x13,
        "esc" | "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "tab" => 0x09,
        "backspace" => 0x08,
        "scrolllock" => 0x91,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.as_str()) {
                (Some(c), "") if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as u32,
                (Some('f'), n) => match n.parse::<u32>() {
                    Ok(n @ 1..=24) => 0x70 + n - 1,
                    _ => return None,
                },
                _ => return None,
            }
        }
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hotkeys() {
        assert_eq!(
            parse_hotkey("Ctrl+Alt+F12"),
            Ok((MOD_CONTROL | MOD_ALT, 0x7B))
        );
        assert_eq!(
            parse_hotkey("ctrl + shift + S"),
            Ok((MOD_CONTROL | MOD_SHIFT, b'S' as u32))
        );
        assert_eq!(parse_hotkey("Win+Pause"), Ok((MOD_WIN, 0x13)));
        assert_eq!(parse_hotkey("Alt+1"), Ok((MOD_ALT, b'1' as u32)));
        assert_eq!(parse_hotkey("Ctrl+F1"), Ok((MOD_CONTROL, 0x70)));
    }

    #[test]
    fn rejects_bad_hotkeys() {
        for bad in [
            "F12",
            "Ctrl+Alt",
            "Ctrl+F25",
            "Ctrl+Щ",
            "Ctrl+A+B",
            "Ctrl+Банан",
            "",
        ] {
            assert!(parse_hotkey(bad).is_err(), "{bad}");
        }
    }
}
