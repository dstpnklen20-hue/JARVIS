//! Сведения о системе: версия ОС, права администратора.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{OwnedHandle, last_error, wide};
use crate::{OsInfo, PlatformError, fix_product_name};

const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

pub fn os_info() -> Result<OsInfo, PlatformError> {
    let build_text = read_reg_string(CURRENT_VERSION_KEY, "CurrentBuildNumber")?;
    let build = build_text.trim().parse().unwrap_or(0);
    let product = read_reg_string(CURRENT_VERSION_KEY, "ProductName")?;
    Ok(OsInfo {
        product: fix_product_name(&product, build),
        display_version: read_reg_string(CURRENT_VERSION_KEY, "DisplayVersion").unwrap_or_default(),
        build,
        revision: read_reg_dword(CURRENT_VERSION_KEY, "UBR").unwrap_or(0),
    })
}

pub fn is_elevated() -> Result<bool, PlatformError> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess возвращает псевдодескриптор, закрывать его
    // не нужно; `raw` — валидный указатель на место для дескриптора токена.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) };
    if ok == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    let token = OwnedHandle(raw);

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0u32;
    // SAFETY: токен открыт с TOKEN_QUERY; буфер — структура TOKEN_ELEVATION,
    // и её точный размер передан вторым аргументом длины.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&raw mut elevation).cast::<c_void>(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    if ok == 0 {
        return Err(last_error("GetTokenInformation"));
    }
    Ok(elevation.TokenIsElevated != 0)
}

fn read_reg_string(key: &str, value: &str) -> Result<String, PlatformError> {
    let key_w = wide(key);
    let value_w = wide(value);

    // Первый вызов узнаёт размер в байтах, второй читает.
    let mut size = 0u32;
    // SAFETY: строки ключа и значения нуль-терминированы и живут до конца
    // вызова; нулевой буфер с указателем на размер — документированный
    // способ запросить требуемую длину.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key_w.as_ptr(),
            value_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(PlatformError::Win32 {
            call: "RegGetValueW",
            code: status,
        });
    }

    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: буфер выделен на `size` байт, размер передан честно; RegGetValueW
    // с RRF_RT_REG_SZ гарантирует нуль-терминатор в пределах буфера.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key_w.as_ptr(),
            value_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast::<c_void>(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(PlatformError::Win32 {
            call: "RegGetValueW",
            code: status,
        });
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..len]))
}

fn read_reg_dword(key: &str, value: &str) -> Result<u32, PlatformError> {
    let key_w = wide(key);
    let value_w = wide(value);
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: строки нуль-терминированы; буфер — u32 размером 4 байта,
    // ровно столько, сколько занимает REG_DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key_w.as_ptr(),
            value_w.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast::<c_void>(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(PlatformError::Win32 {
            call: "RegGetValueW",
            code: status,
        });
    }
    Ok(data)
}

/// Сколько ресурсов занимает текущий процесс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessUsage {
    /// Рабочий набор (занятая физическая память), байты.
    pub memory: u64,
    /// Процессорное время с запуска (ядро + пользователь).
    pub cpu: std::time::Duration,
}

pub fn process_usage() -> Result<ProcessUsage, PlatformError> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: псевдодескриптор текущего процесса не закрывается; структуры
    // валидны для записи, размер передан верный.
    let (times_ok, memory_ok, counters) = unsafe {
        let process = GetCurrentProcess();
        let times_ok = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let memory_ok = GetProcessMemoryInfo(
            process,
            &mut counters,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        );
        (times_ok, memory_ok, counters)
    };
    if times_ok == 0 {
        return Err(last_error("GetProcessTimes"));
    }
    if memory_ok == 0 {
        return Err(last_error("GetProcessMemoryInfo"));
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // FILETIME — сотни наносекунд.
    let cpu = std::time::Duration::from_nanos((ticks(kernel) + ticks(user)) * 100);
    Ok(ProcessUsage {
        memory: counters.WorkingSetSize as u64,
        cpu,
    })
}

#[cfg(test)]
mod usage_tests {
    #[test]
    fn current_process_uses_some_memory_and_cpu() {
        // Немного поработать, чтобы процессорное время было не нулевым.
        let sum: u64 = (0..2_000_000u64).map(|i| i % 7).sum();
        assert!(sum > 0);
        let usage = super::process_usage().unwrap();
        assert!(usage.memory > 1024 * 1024, "{usage:?}");
        assert!(!usage.cpu.is_zero(), "{usage:?}");
    }
}
