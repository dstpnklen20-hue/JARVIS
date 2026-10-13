//! Короткие (8.3) имена путей: нативные библиотеки (espeak-ng внутри
//! sherpa-onnx) открывают файлы старым API, который не понимает русские
//! буквы в пути, — короткое имя всегда латинское.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

/// Короткое имя существующего пути; если его нет (короткие имена отключены
/// на томе, путь не существует) — исходный путь.
pub fn short_path(path: &Path) -> PathBuf {
    let long: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: строка нуль-терминирована, буфер выделен на buf.len() символов.
    let n = unsafe { GetShortPathNameW(long.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize >= buf.len() {
        return path.to_path_buf();
    }
    PathBuf::from(OsString::from_wide(&buf[..n as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyrillic_path_gets_ascii_short_name() {
        let dir = std::env::temp_dir().join(format!("Джарвис тест {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let short = short_path(&dir);
        let shown = short.to_string_lossy().into_owned();
        std::fs::remove_dir_all(&dir).unwrap();
        // На томах с выключенными короткими именами путь остаётся прежним.
        if shown != dir.to_string_lossy() {
            assert!(shown.is_ascii(), "{shown}");
        }
        let missing = Path::new(r"Z:\нет такого");
        assert_eq!(short_path(missing), missing);
    }
}
