//! `jarvis --check`: проверка окружения, конфига и ресурсов.
//!
//! Работает и с битым конфигом — именно чтобы показать, что в нём не так.
//! Код возврата ненулевой, если есть хотя бы одна ошибка (FAIL).

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use cpal::traits::{DeviceTrait as _, HostTrait as _};
use jarvis_audio::capture::Capture;
use jarvis_audio::models::SpeechModels;
use jarvis_audio::pack::VoicePack;
use jarvis_audio::speaker::Voiceprint;
use jarvis_core::assets::{Asset, AssetStatus, MARKER_FILE, Manifest};
use jarvis_core::commands::Registry;
use jarvis_core::config::{DEFAULT_FILE, USER_FILE, resolve};
use jarvis_core::permissions::{PERMISSIONS_FILE, PermissionStore};
use jarvis_core::root::{self, project_path};
use jarvis_core::state::Reply;
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
    if let Some(config) = &config {
        items.push(check_signal(config));
    }

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

    if let Some(config) = &config {
        items.push(check_voice_pack(&root, config));
        items.push(check_tts(&root, config));
        items.push(check_voiceprint(&root, config));
        items.push(check_commands(&root, config));
        items.push(check_permissions(&root));
        items.push(check_llm(config));
        items.push(match crate::daemon::check_hotkey(config) {
            Ok(Some(spec)) => Item::new(
                Status::Ok,
                "Аварийная остановка",
                format!("{spec} и «Джарвис, стоп»"),
            ),
            Ok(None) => Item::new(
                Status::Warn,
                "Аварийная остановка",
                "горячая клавиша выключена — только «Джарвис, стоп» и трей",
            ),
            Err(e) => Item::new(
                Status::Fail,
                "Аварийная остановка",
                format!("hotkeys.emergency_stop: {e}"),
            ),
        });
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

fn check_voice_pack(root: &Path, config: &Config) -> Item {
    const TITLE: &str = "Звуковой пакет";
    let dir = resolve(root, &config.paths.sounds).join(&config.voice.pack);
    match VoicePack::load(&dir) {
        Ok(pack) => {
            let replies: Vec<String> = Reply::ALL
                .iter()
                .map(|r| format!("{} ×{}", r.key(), pack.variants(*r).len()))
                .collect();
            let greetings: Vec<String> = jarvis_audio::pack::DayPart::ALL
                .iter()
                .map(|p| (p.key(), pack.greetings(*p).len()))
                .filter(|(_, n)| *n > 0)
                .map(|(k, n)| format!("{k} ×{n}"))
                .collect();
            let notes = if greetings.is_empty() {
                vec!["приветствий при запуске нет".to_owned()]
            } else {
                vec![format!("приветствия: {}", greetings.join(", "))]
            };
            Item::new(
                Status::Ok,
                TITLE,
                format!("{}: {}", pack.name, replies.join(", ")),
            )
            .with_notes(notes)
        }
        Err(e) => Item::new(Status::Fail, TITLE, error_chain(&e)),
    }
}

/// Голос синтеза: установлен ли и синтезирует ли (короткая фраза).
fn check_tts(root: &Path, config: &Config) -> Item {
    const TITLE: &str = "Синтез речи";
    let Some(name) = config.voice.tts_voice() else {
        return Item::new(
            Status::Skip,
            TITLE,
            "выключен (voice.tts = \"none\") — динамические ответы звуковым пакетом",
        );
    };
    let installed = jarvis_audio::models::installed_voices(root, config).join(", ");
    let model = match jarvis_audio::models::tts_voice(root, config, name) {
        Ok(model) => model,
        Err(e) => {
            return Item::new(
                Status::Warn,
                TITLE,
                format!(
                    "голос «{name}» не установлен — ответы звуковым пакетом ({})",
                    error_chain(&e)
                ),
            )
            .with_notes(vec![format!(
                "установлены: {}; остальные голоса: scripts\\download-models.ps1 -Full",
                if installed.is_empty() {
                    "нет"
                } else {
                    &installed
                }
            )]);
        }
    };
    let started = std::time::Instant::now();
    let result = jarvis_audio::tts::PiperTts::new(&model, config.voice.tts_speed).and_then(|tts| {
        use jarvis_audio::tts::TextToSpeech as _;
        let load = started.elapsed();
        let t = std::time::Instant::now();
        tts.synthesize("Проверка голоса.")
            .map(|clip| (load, t.elapsed(), clip.duration_secs()))
    });
    match result {
        Ok((load, synth, secs)) => Item::new(
            Status::Ok,
            TITLE,
            format!(
                "голос {name}, темп {}: загрузка {} мс, фраза {secs:.1} с за {} мс",
                config.voice.tts_speed,
                load.as_millis(),
                synth.as_millis()
            ),
        )
        .with_notes(vec![format!("установлены голоса: {installed}")]),
        Err(e) => Item::new(Status::Fail, TITLE, error_chain(&e)),
    }
}

fn check_commands(root: &Path, config: &Config) -> Item {
    const TITLE: &str = "Команды";
    let registry = Registry::load(&resolve(root, &config.paths.commands));
    let packs: std::collections::BTreeSet<&str> =
        registry.commands.iter().map(|c| c.pack.as_str()).collect();
    let summary = format!(
        "{} команд в пакетах: {}",
        registry.commands.len(),
        packs.into_iter().collect::<Vec<_>>().join(", ")
    );
    let mut notes: Vec<String> = registry
        .errors
        .iter()
        .map(|e| format!("пропущено: {e}"))
        .collect();
    notes.extend(registry.warnings.iter().cloned());
    let status = match (
        registry.commands.is_empty(),
        registry.errors.is_empty(),
        registry.warnings.is_empty(),
    ) {
        (true, _, _) => Status::Fail,
        (false, false, _) | (false, true, false) => Status::Warn,
        (false, true, true) => Status::Ok,
    };
    Item::new(status, TITLE, summary).with_notes(notes)
}

fn check_llm(config: &Config) -> Item {
    const TITLE: &str = "Нейросеть";
    let key = jarvis_llm::secrets::api_key();
    let key_text = match &key {
        Ok(Some(_)) => "ключ API сохранён".to_owned(),
        Ok(None) => "ключа API нет (jarvis --set-api-key)".to_owned(),
        Err(e) => format!("хранилище ключей: {}", error_chain(e)),
    };
    let llm = &config.llm;
    match (llm.enabled, &key) {
        (false, _) => Item::new(
            Status::Skip,
            TITLE,
            format!("выключена ([llm] enabled = false); {key_text}"),
        ),
        (true, Ok(Some(_))) => Item::new(
            Status::Ok,
            TITLE,
            format!("{}, усилие {}; {key_text}", llm.model, llm.effort),
        ),
        (true, _) => Item::new(Status::Warn, TITLE, format!("включена, но {key_text}")),
    }
}

fn check_permissions(root: &Path) -> Item {
    const TITLE: &str = "Разрешения";
    let path = project_path(root, PERMISSIONS_FILE);
    match PermissionStore::load(&path) {
        Ok(store) if store.remembered() == 0 => Item::new(
            Status::Ok,
            TITLE,
            "пока ни одного — Джарвис спросит перед первой командой",
        ),
        Ok(store) => Item::new(
            Status::Ok,
            TITLE,
            format!(
                "запомнено решений: {} ({PERMISSIONS_FILE})",
                store.remembered()
            ),
        ),
        Err(e) => Item::new(Status::Fail, TITLE, error_chain(&e)),
    }
}

fn check_voiceprint(root: &Path, config: &Config) -> Item {
    const TITLE: &str = "Образец голоса";
    if !config.speaker.verify {
        return Item::new(
            Status::Skip,
            TITLE,
            "проверка голоса выключена (speaker.verify)",
        );
    }
    let models = match SpeechModels::locate(root, config) {
        Ok(m) => m,
        Err(_) => return Item::new(Status::Skip, TITLE, "не проверен: нет моделей"),
    };
    match Voiceprint::load(&crate::hearing::data_dir(root, config), &models.speaker) {
        Ok(Some(print)) => Item::new(
            Status::Ok,
            TITLE,
            format!("{} образцов, порог {:.2}", print.samples, print.threshold),
        ),
        Ok(None) => Item::new(
            Status::Warn,
            TITLE,
            "не записан — Джарвис откликнется на любой голос; запишите свой: jarvis --enroll",
        ),
        Err(e) => Item::new(Status::Fail, TITLE, error_chain(&e)),
    }
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

/// Короткая запись с микрофона: доходит ли звук вообще.
fn check_signal(config: &Config) -> Item {
    const TITLE: &str = "Сигнал";
    let mut capture = match Capture::open(config.audio.input_device_name()) {
        Ok(c) => c,
        Err(e) => return Item::new(Status::Fail, TITLE, error_chain(&e)),
    };
    std::thread::sleep(std::time::Duration::from_millis(600));
    let mut buf = vec![0.0f32; capture.available().max(1)];
    let n = capture.read(&mut buf);
    let peak = buf[..n].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    signal_item(n, peak)
}

fn signal_item(samples: usize, peak: f32) -> Item {
    const TITLE: &str = "Сигнал";
    let db = 20.0 * peak.max(1e-6).log10();
    if samples == 0 {
        Item::new(Status::Fail, TITLE, "микрофон открыт, но не присылает звук")
    } else if peak < 1e-5 {
        // Даже тихая комната даёт шум около −70…−50 dBFS; меньше −100 —
        // цифровая тишина: микрофон выключен на устройстве или в драйвере.
        Item::new(
            Status::Warn,
            TITLE,
            format!(
                "цифровая тишина ({db:.0} dBFS) — микрофон выключен кнопкой на устройстве или в драйвере"
            ),
        )
    } else if db < -70.0 {
        Item::new(
            Status::Warn,
            TITLE,
            format!("очень тихо ({db:.0} dBFS) — проверьте громкость микрофона в Windows"),
        )
    } else {
        Item::new(
            Status::Ok,
            TITLE,
            format!("есть, пик {db:.0} dBFS за 0.6 с"),
        )
    }
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
    fn signal_levels_are_classified() {
        assert_eq!(signal_item(0, 0.0).status, Status::Fail);
        assert_eq!(signal_item(100, 0.0).status, Status::Warn);
        assert_eq!(signal_item(100, 0.0001).status, Status::Warn);
        assert_eq!(signal_item(100, 0.05).status, Status::Ok);
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
