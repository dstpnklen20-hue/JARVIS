//! Запуск программ и скриптов: список аргументов без командной оболочки,
//! тайм-аут и отмена. Скрипт помещается в Job Object, чтобы при тайм-ауте
//! или аварийной остановке завершить всё дерево его процессов.

use std::io::Read as _;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Globalization::{GetOEMCP, MultiByteToWideChar};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};

use super::OwnedHandle;

/// Не показывать окно консоли у скрипта.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Сколько вывода хранить для журнала.
const OUTPUT_LIMIT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    Exited(i32),
    TimedOut,
    Cancelled,
}

fn command(
    program: &Path,
    args: &[String],
    working_dir: Option<&Path>,
    hide_window: bool,
) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    if hide_window {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Запустить и не ждать (программы).
pub fn spawn_detached(
    program: &Path,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<(), String> {
    command(program, args, working_dir, false)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("{}: {e}", program.display()))
}

/// Запустить и дождаться (скрипты). `cancel` проверяется каждые 20 мс.
pub fn run_and_wait(
    program: &Path,
    args: &[String],
    working_dir: Option<&Path>,
    hide_window: bool,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<(RunEnd, String), String> {
    let mut child = command(program, args, working_dir, hide_window)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let job = assign_to_job(&child);

    let output = Arc::new(Mutex::new(Vec::new()));
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        let output = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut out) = output.lock() {
                    out.extend_from_slice(&buf[..n]);
                    let excess = out.len().saturating_sub(OUTPUT_LIMIT);
                    out.drain(..excess);
                }
            }
        })
    })
    .collect();

    let started = Instant::now();
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) => break RunEnd::Exited(status.code().unwrap_or(-1)),
            Ok(None) => {}
            Err(e) => return Err(format!("ожидание процесса: {e}")),
        }
        let stop = if cancel.load(Ordering::Acquire) {
            Some(RunEnd::Cancelled)
        } else if started.elapsed() >= timeout {
            Some(RunEnd::TimedOut)
        } else {
            None
        };
        if let Some(end) = stop {
            kill_tree(&mut child, job.as_ref());
            break end;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    for reader in readers {
        let _ = reader.join();
    }
    let text = output
        .lock()
        .map(|o| decode_console(&o).trim().to_owned())
        .unwrap_or_default();
    Ok((end, text))
}

/// Вывод консольных программ: UTF-8, если он корректен, иначе — OEM-кодовая
/// страница системы (cp866 на русской Windows), иначе — с заменой символов.
fn decode_console(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    // SAFETY: без аргументов.
    let codepage = unsafe { GetOEMCP() };
    let mut wide = vec![0u16; bytes.len()];
    // SAFETY: входной буфер длины `len`; выходной — не короче входного (в
    // однобайтовой кодировке байт даёт не больше одного UTF-16 символа).
    let written =
        unsafe { MultiByteToWideChar(codepage, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), len) };
    match usize::try_from(written) {
        Ok(n) if n > 0 => String::from_utf16_lossy(&wide[..n]),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Job Object без «убить при закрытии»: программы, которые скрипт запустил
/// и оставил работать, после нормального завершения скрипта не трогаем.
fn assign_to_job(child: &Child) -> Option<OwnedHandle> {
    // SAFETY: создаём безымянный Job Object с атрибутами по умолчанию.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return None;
    }
    let job = OwnedHandle(job);
    let process = child.as_raw_handle();
    // SAFETY: оба дескриптора валидны: job только что создан, process
    // принадлежит живому `child`.
    let ok = unsafe { AssignProcessToJobObject(job.0, process) };
    (ok != 0).then_some(job)
}

fn kill_tree(child: &mut Child, job: Option<&OwnedHandle>) {
    match job {
        // SAFETY: дескриптор Job Object валиден, пока жив `job`.
        Some(job) => unsafe {
            TerminateJobObject(job.0, 1);
        },
        None => {
            let _ = child.kill();
        }
    }
    let _ = child.wait();
}

/// Путь к системной утилите (shutdown.exe) — не из текущей папки.
pub fn system32(exe: &str) -> PathBuf {
    let root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
    root.join("System32").join(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(script: &str, timeout: Duration, cancel: &AtomicBool) -> (RunEnd, String) {
        let args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            script.into(),
        ];
        run_and_wait(
            &system32("WindowsPowerShell\\v1.0\\powershell.exe"),
            &args,
            None,
            true,
            timeout,
            cancel,
        )
        .unwrap()
    }

    #[test]
    fn exit_code_and_output() {
        let (end, out) = ps(
            "Write-Output hello; exit 3",
            Duration::from_secs(20),
            &AtomicBool::new(false),
        );
        assert_eq!(end, RunEnd::Exited(3));
        assert!(out.contains("hello"), "{out}");
    }

    #[test]
    fn timeout_kills_process() {
        let started = Instant::now();
        let (end, _) = ps(
            "Start-Sleep -Seconds 30",
            Duration::from_millis(1500),
            &AtomicBool::new(false),
        );
        assert_eq!(end, RunEnd::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn cancel_kills_process() {
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            c2.store(true, Ordering::Release);
        });
        let (end, _) = ps("Start-Sleep -Seconds 30", Duration::from_secs(60), &cancel);
        assert_eq!(end, RunEnd::Cancelled);
        t.join().unwrap();
    }

    #[test]
    fn console_output_in_oem_codepage_is_decoded() {
        // «Привет» в cp866.
        let cp866 = [0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2];
        // SAFETY: без аргументов.
        if unsafe { GetOEMCP() } == 866 {
            assert_eq!(decode_console(&cp866), "Привет");
        }
        assert_eq!(decode_console("уже utf-8".as_bytes()), "уже utf-8");
    }

    #[test]
    fn missing_program_is_an_error() {
        let err = run_and_wait(
            Path::new("C:\\нет\\такой\\программы.exe"),
            &[],
            None,
            true,
            Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.contains("программы.exe"), "{err}");
    }
}
