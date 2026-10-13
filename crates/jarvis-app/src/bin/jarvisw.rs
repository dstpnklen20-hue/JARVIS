//! `jarvisw.exe` — запуск Джарвиса без окна консоли: для ярлыка в меню
//! «Пуск», автозапуска при входе и `jarvis-start.cmd`. Сам `jarvis.exe`
//! остаётся консольным — так в терминале работают `--check`, `--ask`,
//! ввод ключа и запись голоса. Аргументы передаются как есть.
#![windows_subsystem = "windows"]

use std::process::{Command, ExitCode};

#[cfg(windows)]
fn main() -> ExitCode {
    use std::os::windows::process::CommandExt as _;
    /// Процесс без окна консоли.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Ok(me) = std::env::current_exe() else {
        return ExitCode::FAILURE;
    };
    let jarvis = me.with_file_name("jarvis.exe");
    match Command::new(jarvis)
        .args(std::env::args_os().skip(1))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    let _ = Command::new("jarvis");
    ExitCode::FAILURE
}
