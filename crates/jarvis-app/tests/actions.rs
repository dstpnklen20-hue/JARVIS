#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты: падение = провал теста

//! Сквозной тест исполнителей с настоящей системой: скрипт из пакета
//! запускается через PowerShell, слот подставляется в аргумент.

use std::path::Path;

use jarvis_actions::{CancelToken, Context, Outcome, execute};
use jarvis_app::system::PlatformSystem;
use jarvis_core::commands::{ActionSpec, Registry};
use jarvis_core::slots::SlotValues;

#[test]
fn example_script_runs_with_slot_value() {
    let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../commands/examples");
    let registry = Registry::load(&pack.join(".."));
    let command = registry.get("example_note").expect("пример есть в пакетах");

    // Копия скрипта во временной папке — не пачкаем репозиторий.
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(pack.join("note.ps1"), dir.path().join("note.ps1")).unwrap();

    let slots: SlotValues = [("mood".to_owned(), "good".to_owned())]
        .into_iter()
        .collect();
    let action = command.action.with_slots(&slots);
    assert!(matches!(&action, ActionSpec::RunScript { args, .. } if args == &["good"]));

    let cancel = CancelToken::default();
    let play = |_| {};
    let ctx = Context {
        dir: dir.path(),
        system: &PlatformSystem,
        cancel: &cancel,
        play: &play,
    };
    assert_eq!(execute(&action, &ctx), Ok(Outcome::Done));
    let notes = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
    assert!(notes.trim_end().ends_with("good"), "{notes}");
}

#[test]
fn failing_script_reports_exit_code_and_output() {
    let dir = tempfile::tempdir().unwrap();
    // Windows PowerShell 5.1 читает .ps1 без BOM как ANSI — пишем с BOM, как
    // советуем в README.
    std::fs::write(
        dir.path().join("fail.ps1"),
        "\u{FEFF}Write-Output 'что-то пошло не так'; exit 7",
    )
    .unwrap();
    let action = ActionSpec::RunScript {
        path: "fail.ps1".into(),
        args: vec![],
        timeout_sec: Some(20),
    };
    let cancel = CancelToken::default();
    let play = |_| {};
    let ctx = Context {
        dir: dir.path(),
        system: &PlatformSystem,
        cancel: &cancel,
        play: &play,
    };
    let err = execute(&action, &ctx).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("кодом 7"), "{text}");
    assert!(
        text.contains("что-то пошло не так"),
        "вывод скрипта читаемый: {text}"
    );
}
