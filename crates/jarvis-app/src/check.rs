//! `jarvis --check`: проверка окружения, конфига и ресурсов.
//!
//! Работает и с битым конфигом — именно чтобы показать, что в нём не так.
//! Код возврата ненулевой, если есть хотя бы одна ошибка (FAIL).

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use cpal::traits::{DeviceTrait as _, HostTrait as _};
use jarvis_core::assets::{Asset, AssetStatus, MARKER_FILE, Manifest};
use jarvis_core::config::{DEFAULT_FILE, USER_FILE};
use jarvis_core::root::{self, project_path};
use jarvis_core::{Config, error_chain};
use jarvis_platform::{MIN_SUPPORTED_BUILD, OsInfo};

const DOWNLOAD_HINT: &str = r"запустите scripts\download-models.ps1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Fail,
    /// Необязательная часть отсутствует — не ошибка.
    Skip,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Status::Ok => "[ OK ]",
            Status::Warn => "[WARN]",
            Status::Fail => "[FAIL]",
            Status::Skip => "[ -- ]",
        })
    }
}

#[derive(Debug)]
struct Item {
    status: Status,
    title: String,
    detail: String,
    /// Дополнительные строки под пунктом (списки устройств и т. п.).
    notes: Vec<String>,
}

impl Item {
    fn new(status: Status, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            title: title.into(),
            detail: detail.into(),
            notes: Vec::new(),
        }
    }

    fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }
}

pub fn run(explicit_root: Option<&Path>) -> ExitCode {
    let (header, items) = collect(explicit_root);
    print!("{}", render(&header, &items));
    if items.iter().any(|i| i.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn collect(explicit_root: Option<&Path>) -> (String, Vec<Item>) {
    let mut items = vec![check_os(), check_elevation()];

    let root = match root::find_root(explicit_root) {
        Ok(root) => root,
        Err(e) => {
            items.push(Item::new(Status::Fail, "Корень проекта", error_chain(&e)));
            return ("Корень проекта не найден".into(), items);
        }
    };
    let header = format!("Корень проекта: {}", root.display());

    let config = match Config::load(&root) {
        Ok(config) => {
            let sources = if project_path(&root, USER_FILE).is_file() {
                format!("{DEFAULT_FILE} + {USER_FILE}")
            } else {
                format!("{DEFAULT_FILE} ({USER_FILE} нет — всё по умолчанию)")
            };
            items.push(Item::new(Status::Ok, "Конфиг", sources));
            if config.general.dry_run {
                items.push(Item::new(
                    Status::Warn,
                    "Dry-run",
                    "включён: команды не будут выполняться",
                ));
            }
            Some(config)
        }
        Err(e) => {
            items.push(Item::new(Status::Fail, "Конфиг", error_chain(&e)));
            None
        }
    };

    items.push(check_microphone(config.as_ref()));

    match (Manifest::load(&root), &config) {
        (Ok(manifest), Some(config)) => {
            items.extend(
                manifest
                    .assets
                    .iter()
                    .map(|a| check_asset(a, &root, config)),
            );
        }
        (Err(e), _) => items.push(Item::new(Status::Fail, "Ресурсы", error_chain(&e))),
        (Ok(_), None) => items.push(Item::new(
            Status::Skip,
            "Ресурсы",
            "не проверены: сначала исправьте конфиг",
        )),
    }

    (header, items)
}

fn check_asset(asset: &Asset, root: &Path, config: &Config) -> Item {
    let location = asset.location(root, config);
    let shown = location.strip_prefix(root).unwrap_or(&location).display();
    let what = &asset.description;
    let (status, detail) = match asset.status(&location) {
        AssetStatus::Ok => (Status::Ok, format!("{shown} — {what}")),
        AssetStatus::Missing if !asset.required => (
            Status::Skip,
            format!("не скачано — {what}; необязательно ({DOWNLOAD_HINT} -Full)"),
        ),
        AssetStatus::Missing => (
            Status::Fail,
            format!("не скачано — {what}; {DOWNLOAD_HINT}"),
        ),
        AssetStatus::NotVerified => (
            Status::Fail,
            format!(
                "{shown}: нет метки {MARKER_FILE} — распаковано не скриптом или загрузка прервана; {DOWNLOAD_HINT}"
            ),
        ),
        AssetStatus::HashMismatch { found } => (
            Status::Fail,
            format!(
                "{shown}: SHA-256 архива {found} не совпадает с ожидаемым — {DOWNLOAD_HINT} -Force"
            ),
        ),
        AssetStatus::FileMissing(file) => (
            Status::Fail,
            format!("{shown}: нет файла {file} — {DOWNLOAD_HINT} -Force"),
        ),
    };
    // Поломка необязательного ресурса — не повод для ошибки.
    let status = match status {
        Status::Fail if !asset.required => Status::Warn,
        other => other,
    };
    Item::new(status, &asset.name, detail)
}

fn check_os() -> Item {
    match jarvis_platform::os_info() {
        Ok(info) => Item::new(os_status(&info), "ОС", info.to_string()),
        Err(e) => Item::new(Status::Fail, "ОС", error_chain(&e)),
    }
}

fn os_status(info: &OsInfo) -> Status {
    if info.build >= MIN_SUPPORTED_BUILD {
        Status::Ok
    } else {
        Status::Warn
    }
}

fn check_elevation() -> Item {
    match jarvis_platform::is_elevated() {
        Ok(false) => Item::new(Status::Ok, "Права", "обычный пользователь"),
        Ok(true) => Item::new(
            Status::Warn,
            "Права",
            "запущено от администратора — команды тоже получат эти права",
        ),
        Err(e) => Item::new(Status::Warn, "Права", format!("не удалось проверить: {e}")),
    }
}

fn check_microphone(config: Option<&Config>) -> Item {
    const TITLE: &str = "Микрофон";
    let host = cpal::default_host();
    let names: Vec<String> = match host.input_devices() {
        Ok(devices) => devices.map(|d| d.to_string()).collect(),
        Err(e) => {
            return Item::new(
                Status::Fail,
                TITLE,
                format!("не удалось получить список устройств: {e}"),
            );
        }
    };
    let notes = std::iter::once("устройства ввода:".to_owned())
        .chain(names.iter().map(|n| format!("  - {n}")))
        .collect();

    if names.is_empty() {
        return Item::new(Status::Fail, TITLE, "в системе нет ни одного микрофона");
    }

    let wanted = config.and_then(|c| c.audio.input_device_name());
    let chosen = match wanted {
        None => host.default_input_device().map(|d| d.to_string()),
        Some(part) => find_device(&names, part).map(str::to_owned),
    };

    match (chosen, wanted) {
        (Some(name), None) => Item::new(
            Status::Ok,
            TITLE,
            format!(
                "системный по умолчанию: {name}{}",
                describe_config(&host, &name)
            ),
        ),
        (Some(name), Some(part)) => Item::new(
            Status::Ok,
            TITLE,
            format!("«{part}» → {name}{}", describe_config(&host, &name)),
        ),
        (None, None) => Item::new(
            Status::Fail,
            TITLE,
            "системный микрофон по умолчанию не выбран в Windows",
        ),
        (None, Some(part)) => Item::new(
            Status::Fail,
            TITLE,
            format!("устройство «{part}» из audio.input_device не найдено"),
        ),
    }
    .with_notes(notes)
}

/// Первое устройство, имя которого содержит `part` без учёта регистра.
fn find_device<'a>(names: &'a [String], part: &str) -> Option<&'a str> {
    let part = part.to_lowercase();
    names
        .iter()
        .find(|n| n.to_lowercase().contains(&part))
        .map(String::as_str)
}

/// Формат захвата устройства по умолчанию, например «, 48000 Гц, 2 кан.».
fn describe_config(host: &cpal::Host, name: &str) -> String {
    let device = host
        .input_devices()
        .ok()
        .and_then(|mut it| it.find(|d| d.to_string() == name));
    match device.map(|d| d.default_input_config()) {
        Some(Ok(cfg)) => format!(
            " ({} Гц, {} кан., {})",
            cfg.sample_rate(),
            cfg.channels(),
            cfg.sample_format()
        ),
        _ => String::new(),
    }
}

fn render(header: &str, items: &[Item]) -> String {
    let width = items
        .iter()
        .map(|i| i.title.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!(
        "Джарвис {} — проверка окружения\n{header}\n\n",
        env!("CARGO_PKG_VERSION")
    );
    for item in items {
        let pad = width - item.title.chars().count();
        // Многострочные ошибки конфига выравниваем под текст пункта.
        let indent = " ".repeat(6 + 1 + width + 2);
        let detail = item.detail.replace('\n', &format!("\n{indent}"));
        out.push_str(&format!(
            "{} {}{}  {detail}\n",
            item.status,
            item.title,
            " ".repeat(pad)
        ));
        for note in &item.notes {
            out.push_str(&format!("{indent}{note}\n"));
        }
    }
    let count = |s| items.iter().filter(|i| i.status == s).count();
    let (fails, warns) = (count(Status::Fail), count(Status::Warn));
    out.push('\n');
    out.push_str(&if fails == 0 && warns == 0 {
        "Итог: всё в порядке.\n".to_owned()
    } else {
        format!("Итог: ошибок — {fails}, предупреждений — {warns}.\n")
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_is_found_by_case_insensitive_part() {
        let names = vec![
            "Микрофон (ME6S)".to_owned(),
            "Микрофон (Fuxi-H3 )".to_owned(),
        ];
        assert_eq!(find_device(&names, "fuxi"), Some("Микрофон (Fuxi-H3 )"));
        assert_eq!(find_device(&names, "me6s"), Some("Микрофон (ME6S)"));
        assert_eq!(find_device(&names, "микрофон"), Some("Микрофон (ME6S)"));
        assert_eq!(find_device(&names, "Blue Yeti"), None);
    }

    #[test]
    fn render_summarises_and_aligns() {
        let items = vec![
            Item::new(Status::Ok, "ОС", "Windows"),
            Item::new(Status::Fail, "Конфиг", "строка 1\nстрока 2"),
            Item::new(Status::Warn, "Права", "админ").with_notes(vec!["заметка".into()]),
        ];
        let text = render("Корень проекта: X", &items);
        assert!(text.contains("[ OK ] ОС      Windows"), "{text}");
        // Отступ = "[FAIL] " (7) + ширина заголовка (6) + 2 пробела.
        let indent = " ".repeat(15);
        assert!(
            text.contains(&format!("[FAIL] Конфиг  строка 1\n{indent}строка 2")),
            "{text}"
        );
        assert!(text.contains(&format!("\n{indent}заметка\n")), "{text}");
        assert!(
            text.ends_with("Итог: ошибок — 1, предупреждений — 1.\n"),
            "{text}"
        );
    }

    #[test]
    fn render_all_ok() {
        let text = render("h", &[Item::new(Status::Ok, "ОС", "Windows")]);
        assert!(text.ends_with("Итог: всё в порядке.\n"));
    }

    #[test]
    fn missing_root_is_a_failure() {
        let empty = std::env::temp_dir().join("jarvis-check-no-root-here");
        let (_, items) = collect(Some(&empty));
        let last = items.last().unwrap();
        assert_eq!(last.status, Status::Fail);
        assert_eq!(last.title, "Корень проекта");
    }
}
