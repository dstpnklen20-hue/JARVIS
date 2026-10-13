//! Автозапуск при входе в Windows: значение в `HKCU\...\Run` — только для
//! текущего пользователя, без прав администратора.

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use super::wide;
use crate::PlatformError;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn win32(call: &'static str, code: u32) -> PlatformError {
    PlatformError::Win32 { call, code }
}

/// Команда автозапуска с этим именем, если есть.
pub fn autostart_command(name: &str) -> Result<Option<String>, PlatformError> {
    let key = wide(RUN_KEY);
    let value = wide(name);
    let mut size = 0u32;
    // SAFETY: запрос размера: буфер не передаётся (null), size — валидный указатель.
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if code != ERROR_SUCCESS {
        return Err(win32("RegGetValueW", code));
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: буфер выделен на `size` байт, размер передан честно; с
    // RRF_RT_REG_SZ строка завершается нулём в пределах буфера.
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32("RegGetValueW", code));
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(Some(String::from_utf16_lossy(&buf[..len])))
}

/// Включить автозапуск командой `command` или выключить (`None`).
pub fn set_autostart(name: &str, command: Option<&str>) -> Result<(), PlatformError> {
    let key = wide(RUN_KEY);
    let value = wide(name);
    let code = match command {
        Some(command) => {
            let data = wide(command);
            // SAFETY: строки нуль-терминированы и живут до конца вызова;
            // размер данных — в байтах, с нулём в конце.
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    value.as_ptr(),
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            }
        }
        None => {
            // SAFETY: строки нуль-терминированы и живут до конца вызова.
            let code =
                unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
            if code == ERROR_FILE_NOT_FOUND {
                ERROR_SUCCESS
            } else {
                code
            }
        }
    };
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(win32("RegSetKeyValueW", code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_value_roundtrip() {
        // Отдельное имя — настоящий автозапуск Джарвиса не трогаем.
        let name = format!("JarvisTest{}", std::process::id());
        assert_eq!(autostart_command(&name).unwrap(), None);
        set_autostart(&name, Some(r#""C:\Программы\jarvisw.exe""#)).unwrap();
        assert_eq!(
            autostart_command(&name).unwrap().as_deref(),
            Some(r#""C:\Программы\jarvisw.exe""#)
        );
        set_autostart(&name, None).unwrap();
        assert_eq!(autostart_command(&name).unwrap(), None);
        set_autostart(&name, None).unwrap(); // повторное выключение — не ошибка
    }
}
