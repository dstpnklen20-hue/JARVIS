//! `jarvis` — демон голосового ассистента.

mod check;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use jarvis_core::{Config, logging, root};

#[derive(Debug, Parser)]
#[command(
    name = "jarvis",
    version,
    about = "Джарвис — голосовой ассистент для ПК"
)]
struct Cli {
    /// Проверить окружение, конфиг и модели, напечатать отчёт и выйти.
    #[arg(long)]
    check: bool,

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

    tracing::info!(root = %root.display(), version = env!("CARGO_PKG_VERSION"), "Джарвис запущен");
    warn_if_elevated();
    if config.general.dry_run {
        tracing::warn!("режим dry-run: команды будут распознаваться, но не выполняться");
    }

    // Этап 0: только каркас. Прослушивание микрофона появится на этапе 1.
    tracing::info!("каркас готов; проверка окружения: jarvis --check");
    Ok(())
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
