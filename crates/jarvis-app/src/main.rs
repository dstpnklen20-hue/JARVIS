//! `jarvis` — демон голосового ассистента.

mod bench;
mod check;
mod core;
mod daemon;
mod enroll;
mod hearing;
mod record;
mod say;
mod tray;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{ArgGroup, Parser};
use jarvis_core::{Config, logging, root};

#[derive(Debug, Parser)]
#[command(
    name = "jarvis",
    version,
    about = "Джарвис — голосовой ассистент для ПК",
    group(ArgGroup::new("mode").args(["check", "stop", "bench", "enroll", "enroll_from", "record", "say"]))
)]
struct Cli {
    /// Проверить окружение, конфиг и модели, напечатать отчёт и выйти.
    #[arg(long)]
    check: bool,

    /// Мягко остановить работающего Джарвиса.
    #[arg(long)]
    stop: bool,

    /// Замеры слуха на WAV-файлах (по умолчанию — tests/fixtures).
    #[arg(long, value_name = "WAV", num_args = 0..)]
    bench: Option<Vec<PathBuf>>,

    /// Записать образец вашего голоса с микрофона.
    #[arg(long)]
    enroll: bool,

    /// Записать образец голоса из WAV-файлов (по одному «Джарвис» в файле).
    #[arg(long, value_name = "WAV", num_args = 3..)]
    enroll_from: Option<Vec<PathBuf>>,

    /// Отладка: записать звук с микрофона (16 кГц, как его слышат модели) в WAV.
    #[arg(long, value_name = "WAV")]
    record: Option<PathBuf>,

    /// Длительность записи для --record, секунды.
    #[arg(long, value_name = "SECS", default_value_t = 5, requires = "record")]
    seconds: u32,

    /// Произнести фразу голосом синтеза (послушать голос и темп).
    #[arg(long, value_name = "TEXT")]
    say: Option<String>,

    /// Голос для --say вместо voice.tts: dmitri, irina, ruslan, denis.
    #[arg(long, value_name = "NAME", requires = "say")]
    voice: Option<String>,

    /// Сохранить фразу --say в WAV вместо проигрывания.
    #[arg(long, value_name = "WAV", requires = "say")]
    out: Option<PathBuf>,

    /// Корень проекта (папка с config/). По умолчанию ищется от текущей
    /// папки и от папки программы вверх.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.check {
        return check::run(cli.root.as_deref());
    }
    if cli.stop {
        return match jarvis_platform::send_stop_signal(daemon::INSTANCE_NAME) {
            Ok(true) => {
                println!("Джарвису отправлена команда остановки.");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("Джарвис не запущен.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Ошибка: {}", jarvis_core::error_chain(&e));
                ExitCode::FAILURE
            }
        };
    }
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Ошибка: {}", jarvis_core::error_chain(e.as_ref()));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    let root = root::find_root(cli.root.as_deref())?;
    let config = Config::load(&root).context("конфиг не загружен, запуск отменён")?;
    let _log_guard = logging::init(&config.logging, &root)?;
    install_panic_hook();

    if let Some(files) = &cli.bench {
        return bench::run(&root, &config, files);
    }
    if cli.enroll {
        return enroll::run_microphone(&root, &config);
    }
    if let Some(files) = &cli.enroll_from {
        return enroll::run_files(&root, &config, files);
    }
    if let Some(path) = &cli.record {
        return record::run(&config, path, cli.seconds);
    }
    if let Some(text) = &cli.say {
        return say::run(
            &root,
            &config,
            text,
            cli.voice.as_deref(),
            cli.out.as_deref(),
        );
    }

    tracing::info!(root = %root.display(), version = env!("CARGO_PKG_VERSION"), "Джарвис запущен");
    warn_if_elevated();
    if config.general.dry_run {
        tracing::warn!("режим dry-run: команды будут распознаваться, но не выполняться");
    }
    daemon::run(&root, config)
}

/// Паника пишется в лог (а не только в консоль, которой может не быть).
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("?"),
            panic = %info,
            "паника"
        );
        default(info);
    }));
}

fn warn_if_elevated() {
    match jarvis_platform::is_elevated() {
        Ok(true) => tracing::warn!(
            "Джарвис запущен от имени администратора: скрипты и программы из команд \
             тоже получат права администратора. Запускайте от обычного пользователя, \
             если это не нужно."
        ),
        Ok(false) => {}
        Err(e) => tracing::debug!(error = %e, "не удалось проверить права администратора"),
    }
}
