//! Системные действия: открыть адрес, нажать мультимедиа-клавишу.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_VOLUME_DOWN,
    VK_VOLUME_MUTE, VK_VOLUME_UP,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::wide;

/// Открывает адрес программой по умолчанию (браузером). Адрес передаётся
/// одной строкой-параметром ShellExecute, без командной оболочки.
pub fn open_url(url: &str) -> Result<(), String> {
    let verb = wide("open");
    let target = wide(url);
    // SAFETY: строки нуль-терминированы и живут до конца вызова; окно-владелец,
    // параметры и рабочая папка не нужны (null).
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW: значение > 32 — успех, иначе код ошибки.
    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute вернул код {code}"))
    }
}

/// Мультимедиа-клавиши (виртуальные коды; скан-коды для игр — этап 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
    Stop,
    VolumeUp,
    VolumeDown,
    Mute,
}

fn vk(key: MediaKey) -> VIRTUAL_KEY {
    match key {
        MediaKey::PlayPause => VK_MEDIA_PLAY_PAUSE,
        MediaKey::Next => VK_MEDIA_NEXT_TRACK,
        MediaKey::Previous => VK_MEDIA_PREV_TRACK,
        MediaKey::Stop => VK_MEDIA_STOP,
        MediaKey::VolumeUp => VK_VOLUME_UP,
        MediaKey::VolumeDown => VK_VOLUME_DOWN,
        MediaKey::Mute => VK_VOLUME_MUTE,
    }
}

fn key_input(key: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Нажимает и отпускает клавишу `presses` раз. Нажатие и отпускание идут
/// одним пакетом SendInput — клавиша не может «залипнуть».
pub fn media_key(key: MediaKey, presses: u32) -> Result<(), String> {
    let code = vk(key);
    let inputs: Vec<INPUT> = (0..presses)
        .flat_map(|_| [key_input(code, false), key_input(code, true)])
        .collect();
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
