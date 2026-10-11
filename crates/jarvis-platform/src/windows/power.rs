//! Питание и громкость системы.

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Power::SetSuspendState;
use windows_sys::Win32::System::Shutdown::LockWorkStation;

use super::last_error;
use super::process::{spawn_detached, system32};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Lock,
    Sleep,
    Logoff,
    Restart,
    Shutdown,
}

pub fn power(op: Power) -> Result<(), String> {
    match op {
        Power::Lock => {
            // SAFETY: без аргументов.
            if unsafe { LockWorkStation() } == 0 {
                return Err(last_error("LockWorkStation").to_string());
            }
            Ok(())
        }
        Power::Sleep => {
            // SAFETY: сон (не гибернация), разрешаем события пробуждения.
            if !unsafe { SetSuspendState(false, false, false) } {
                return Err(last_error("SetSuspendState").to_string());
            }
            Ok(())
        }
        // shutdown.exe из System32 — штатный способ без повышения привилегий
        // процесса; аргументы — списком.
        Power::Logoff => spawn_detached(&system32("shutdown.exe"), &["/l".into()], None),
        Power::Restart => spawn_detached(
            &system32("shutdown.exe"),
            &["/r".into(), "/t".into(), "0".into()],
            None,
        ),
        Power::Shutdown => spawn_detached(
            &system32("shutdown.exe"),
            &["/s".into(), "/t".into(), "0".into()],
            None,
        ),
    }
}

/// Громкость устройства вывода по умолчанию, 0–100.
pub fn set_volume(percent: u32) -> Result<(), String> {
    let level = percent.min(100) as f32 / 100.0;
    // SAFETY: COM инициализируется в этом потоке (действие — в своём потоке);
    // S_FALSE/RPC_E_CHANGED_MODE — COM уже инициализирован, тоже годится.
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let result = (|| -> windows::core::Result<()> {
        // SAFETY: стандартная последовательность COM-вызовов Core Audio API;
        // все интерфейсы освобождаются автоматически (Drop у windows-rs).
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            volume.SetMasterVolumeLevelScalar(level, std::ptr::null())?;
        }
        Ok(())
    })();
    if init.is_ok() {
        // SAFETY: парный вызов к успешному CoInitializeEx в этом потоке.
        unsafe { CoUninitialize() };
    }
    result.map_err(|e| e.message().to_string())
}
