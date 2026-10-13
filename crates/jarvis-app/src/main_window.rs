//! Главное окно: страница `ui/index.html` (вшита в exe) в WebView2 через wry
//! поверх окна из `jarvis-platform`. Живёт в главном потоке рядом с треем.
//!
//! Связь со страницей:
//! - страница → Джарвис: `window.ipc.postMessage(JSON)` → [`PageMsg`];
//! - Джарвис → страница: `window.jarvis.update({...})` через `evaluate_script`.
//!
//! Страница не ходит в интернет (CSP в самой странице, переходы и новые
//! окна запрещены здесь). Данные выводятся в ней только через `textContent`.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_core::Config;
use jarvis_platform::{AppWindow, AppWindowSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use jarvis_core::commands::Registry;
use jarvis_core::permissions::{PermissionStore, Verdict};

use crate::tray::Indicator;

const PAGE: &str = include_str!("../ui/index.html");
/// Фон страницы (`--bg` в index.html) — без белой вспышки при открытии.
const BACKGROUND: u32 = 0x05_09_0E;
/// Как часто обновлять «Ресурсы», пока окно открыто.
const RESOURCES_EVERY: Duration = Duration::from_secs(2);
/// Громкость для реактора — ~30 раз в секунду.
const LEVEL_EVERY: Duration = Duration::from_millis(33);

/// Сообщение страницы.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PageMsg {
    /// Страница загрузилась — прислать ей всё.
    Ready,
    /// Открыта вкладка «Команды» — прислать свежий список.
    Commands,
    Autostart {
        on: bool,
    },
    Permission {
        id: String,
        value: String,
    },
    /// Ключ API: в лог не пишется (у типа нет Debug).
    ApiKey {
        key: String,
    },
    /// «Сохранить» на вкладке «Настройки» (поля — `settings::SettingsForm`).
    Settings(Value),
    /// «Перезапустить» после сохранения настроек.
    Restart,
    /// Редактор команд: сохранить (поля — `command_editor::CommandForm`).
    SaveCommand(Value),
    DeleteCommand {
        id: String,
    },
    /// «Выбрать…» у пути программы.
    PickProgram,
}

impl PageMsg {
    /// Название для лога — без содержимого (в нём может быть ключ).
    pub fn kind(&self) -> &'static str {
        match self {
            PageMsg::Ready => "ready",
            PageMsg::Commands => "commands",
            PageMsg::Autostart { .. } => "autostart",
            PageMsg::Permission { .. } => "permission",
            PageMsg::ApiKey { .. } => "api_key",
            PageMsg::Settings(_) => "settings",
            PageMsg::Restart => "restart",
            PageMsg::SaveCommand(_) => "save_command",
            PageMsg::DeleteCommand { .. } => "delete_command",
            PageMsg::PickProgram => "pick_program",
        }
    }
}

pub struct MainWindow {
    // Порядок важен: страница удаляется раньше окна, в котором живёт.
    webview: WebView,
    window: AppWindow,
    ipc: mpsc::Receiver<String>,
    ready: bool,
}

impl MainWindow {
    /// `data_dir` — папка данных WebView2 (кэш, настройки движка).
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let (icon, icon_size) = crate::tray::window_icon();
        let window = jarvis_platform::open_app_window(&AppWindowSpec {
            title: "Джарвис",
            size: (470, 680),
            min_size: (400, 560),
            background: BACKGROUND,
            icon_rgba: Some((&icon, icon_size)),
        })
        .context("окно не создалось")?;
        let (tx, ipc) = mpsc::channel();
        let mut context = WebContext::new(Some(data_dir.to_path_buf()));
        let [r, g, b] = [BACKGROUND >> 16, BACKGROUND >> 8, BACKGROUND].map(|v| (v & 0xFF) as u8);
        let webview = WebViewBuilder::new_with_web_context(&mut context)
            .with_html(PAGE)
            .with_background_color((r, g, b, 255))
            .with_devtools(false)
            // Только своя страница: никаких переходов по ссылкам и новых окон.
            .with_navigation_handler(|url| url.starts_with("data:") || url == "about:blank")
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_default_context_menus(false)
            .with_browser_accelerator_keys(false)
            .with_ipc_handler(move |request: wry::http::Request<String>| {
                let _ = tx.send(request.into_body());
            })
            .build(&window)
            .context("WebView2 не запустился (нужна среда Microsoft Edge WebView2)")?;
        window.set_tick(LEVEL_EVERY.as_millis() as u32);
        Ok(Self {
            webview,
            window,
            ipc,
            ready: false,
        })
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    pub fn bring_to_front(&self) {
        self.window.bring_to_front();
    }

    /// Окно выбора программы поверх главного.
    pub fn pick_program(&self) -> Option<std::path::PathBuf> {
        self.window.pick_program()
    }

    /// Сообщения страницы с прошлого раза.
    pub fn take_messages(&mut self) -> Vec<PageMsg> {
        let mut out = Vec::new();
        while let Ok(text) = self.ipc.try_recv() {
            match serde_json::from_str::<PageMsg>(&text) {
                Ok(msg) => {
                    if matches!(msg, PageMsg::Ready) {
                        self.ready = true;
                    }
                    out.push(msg);
                }
                // Текст сообщения в лог не идёт: в нём может быть ключ.
                Err(e) => tracing::warn!(error = %e, "непонятное сообщение страницы"),
            }
        }
        out
    }

    /// Передать странице данные (поля `window.jarvis.update`).
    pub fn update(&self, data: &Value) {
        if !self.ready {
            return; // всё нужное уйдёт снимком по `ready`
        }
        let script = format!("window.jarvis && window.jarvis.update({data});");
        if let Err(e) = self.webview.evaluate_script(&script) {
            tracing::warn!(error = %e, "страница окна не обновилась");
        }
    }
}

/// Что показывает окно; копится всегда, даже когда окно закрыто.
pub struct WindowModel {
    pub indicator: Indicator,
    pub heard: String,
    pub mic: Option<String>,
    usage: Option<(Instant, Duration)>,
    usage_due: Instant,
    level_sent: f32,
    level_due: Instant,
}

impl Default for WindowModel {
    fn default() -> Self {
        Self {
            indicator: Indicator::Starting,
            heard: String::new(),
            mic: None,
            usage: None,
            usage_due: Instant::now(),
            level_sent: 0.0,
            level_due: Instant::now(),
        }
    }
}

impl WindowModel {
    /// Всё для только что загруженной страницы.
    pub fn snapshot(&mut self, config: &Config) -> Value {
        json!({
            "version": format!("v{}", env!("CARGO_PKG_VERSION")),
            "state": self.indicator.name(),
            "problem": problem_text(&self.indicator),
            "heard": self.heard,
            "status": {
                "mic": self.mic_status(),
                "ai": ai_status(config),
                "res": self.resources(),
            },
        })
    }

    pub fn state_update(&self) -> Value {
        json!({
            "state": self.indicator.name(),
            "problem": problem_text(&self.indicator),
            "status": { "mic": self.mic_status() },
        })
    }

    /// Громкость для реактора, если пора и она изменилась: голос Джарвиса
    /// всегда, микрофон — только когда Джарвис слушает команду (иначе реактор
    /// дёргался бы от телевизора).
    pub fn level_due(&mut self, now: Instant, mic: f32, output: f32) -> Option<Value> {
        if now < self.level_due {
            return None;
        }
        self.level_due = now + LEVEL_EVERY;
        let mic = if self.indicator == Indicator::Listening {
            mic
        } else {
            0.0
        };
        let level = mic.max(output);
        if (level - self.level_sent).abs() < 0.02 {
            return None;
        }
        self.level_sent = level;
        Some(json!({ "level": (f64::from(level) * 100.0).round() / 100.0 }))
    }

    /// «Ресурсы», если пора обновить.
    pub fn resources_due(&mut self, now: Instant) -> Option<Value> {
        if now < self.usage_due {
            return None;
        }
        self.usage_due = now + RESOURCES_EVERY;
        Some(json!({ "status": { "res": self.resources() } }))
    }

    fn mic_status(&self) -> Value {
        match (&self.indicator, &self.mic) {
            (Indicator::Problem(why), _) => json!({ "level": "bad", "text": why }),
            (Indicator::Paused, _) => json!({ "level": "info", "text": "на паузе — освобождён" }),
            (_, Some(name)) => json!({ "level": "ok", "text": name }),
            (_, None) => json!({ "level": "warn", "text": "открывается…" }),
        }
    }

    fn resources(&mut self) -> Value {
        let Ok(usage) = jarvis_platform::process_usage() else {
            return json!({ "level": "info", "text": "нет данных" });
        };
        let now = Instant::now();
        let mb = usage.memory / (1024 * 1024);
        // Доля всего процессора, как в Диспетчере задач.
        let cpu = self.usage.map(|(at, before)| {
            let wall = now.duration_since(at).as_secs_f64();
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
            let busy = usage.cpu.saturating_sub(before).as_secs_f64();
            if wall > 0.0 {
                (busy / wall / cores * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            }
        });
        self.usage = Some((now, usage.cpu));
        // Неразрывные пробелы: строка переносится только после «·».
        let text = match cpu {
            Some(cpu) => format!("RAM\u{a0}{mb}\u{a0}МБ · CPU\u{a0}{cpu:.0}%"),
            None => format!("RAM\u{a0}{mb}\u{a0}МБ"),
        };
        json!({ "level": "info", "text": text })
    }
}

/// Вкладка «Команды»: команды по пакетам с текущим разрешением.
/// `always_ask` — команда спрашивает каждый раз (пометка автора), такую в
/// окне не переключить: снять пометку можно только в permissions.toml.
pub fn commands_json(registry: &Registry, permissions: &PermissionStore) -> Value {
    let mut commands: Vec<_> = registry.commands.iter().collect();
    // Пакеты по алфавиту, внутри — как в манифестах.
    commands.sort_by(|a, b| a.pack.cmp(&b.pack));
    let list: Vec<Value> = commands
        .into_iter()
        .map(|c| {
            let verdict = permissions.verdict(c);
            let permission = match verdict {
                Verdict::Allowed => "always",
                Verdict::Denied => "never",
                Verdict::Ask { .. } => "ask",
            };
            let locked = matches!(
                verdict,
                Verdict::Ask {
                    offer_always: false
                }
            );
            let form = crate::command_editor::form_of(c);
            json!({
                "id": c.id,
                "pack": c.pack,
                "phrases": c.phrases,
                "action": c.description,
                "permission": permission,
                "always_ask": locked,
                "editable": form.is_some(),
                "form": form,
            })
        })
        .collect();
    json!({ "commands": list })
}

fn problem_text(indicator: &Indicator) -> Value {
    match indicator {
        Indicator::Problem(why) => json!(why),
        _ => Value::Null,
    }
}

/// «Нейросети»: распознавание всегда; Claude — включён ли и есть ли ключ.
pub fn ai_status(config: &Config) -> Value {
    let llm = &config.llm;
    if !llm.enabled {
        return json!({ "level": "ok", "text": "Zipformer · Claude выключен" });
    }
    let model = model_title(&llm.model);
    match jarvis_llm::secrets::api_key() {
        Ok(Some(_)) => json!({ "level": "ok", "text": format!("Zipformer · {model}") }),
        Ok(None) => json!({ "level": "warn", "text": format!("Zipformer · {model}: нет ключа") }),
        Err(_) => json!({ "level": "warn", "text": "Zipformer · хранилище ключей недоступно" }),
    }
}

/// `claude-sonnet-5-5` → «Claude Sonnet 5.5».
fn model_title(id: &str) -> String {
    let rest = id.strip_prefix("claude-").unwrap_or(id);
    let mut words = rest.split('-');
    let family = words.next().unwrap_or_default();
    let version: Vec<&str> = words.collect();
    let mut name = String::from("Claude ");
    let mut chars = family.chars();
    if let Some(first) = chars.next() {
        name.extend(first.to_uppercase());
        name.push_str(chars.as_str());
    }
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_titles_read_like_names() {
        assert_eq!(model_title("claude-sonnet-5-5"), "Claude Sonnet 5.5");
        assert_eq!(model_title("claude-sonnet-5"), "Claude Sonnet 5");
        assert_eq!(model_title("claude-opus-4-8"), "Claude Opus 4.8");
    }

    #[test]
    fn page_messages_parse_and_hide_key_from_log_name() {
        let msg: PageMsg = serde_json::from_str(r#"{"type":"api_key","key":"sk-secret"}"#).unwrap();
        assert_eq!(msg.kind(), "api_key");
        let msg: PageMsg =
            serde_json::from_str(r#"{"type":"permission","id":"open_youtube","value":"always"}"#)
                .unwrap();
        assert!(
            matches!(msg, PageMsg::Permission { ref id, ref value } if id == "open_youtube" && value == "always")
        );
        assert!(serde_json::from_str::<PageMsg>(r#"{"type":"rm -rf"}"#).is_err());
    }

    #[test]
    fn form_messages_reach_forms_without_type_field() {
        // Формы запрещают лишние поля: тег сообщения не должен в них попасть.
        let msg: PageMsg = serde_json::from_str(
            r#"{"type":"settings","mic":"default","pack":"jarvis","tts":"dmitri","volume":100,"llm":true,"show_window":true,"follow_up_secs":12,"emergency_stop":"Ctrl+Alt+F12","dry_run":false,"greet":true,"voice_check":true,"threshold":"auto"}"#,
        )
        .unwrap();
        let PageMsg::Settings(values) = msg else {
            panic!("ожидали Settings");
        };
        let form: crate::settings::SettingsForm = serde_json::from_value(values).unwrap();
        assert_eq!(form.pack, "jarvis");

        let msg: PageMsg = serde_json::from_str(
            r#"{"type":"save_command","phrases":["открой почту"],"description":"откроет почту","always_ask":false,"reply":"ok","action":{"type":"open_url","url":"https://mail.google.com","find_tab":[]}}"#,
        )
        .unwrap();
        let PageMsg::SaveCommand(values) = msg else {
            panic!("ожидали SaveCommand");
        };
        let form: crate::command_editor::CommandForm = serde_json::from_value(values).unwrap();
        assert_eq!(form.phrases, ["открой почту"]);
        assert!(form.id.is_none());
    }

    #[test]
    fn state_update_carries_problem_and_mic() {
        let mut model = WindowModel {
            indicator: Indicator::Problem("микрофон отключён".into()),
            ..WindowModel::default()
        };
        let update = model.state_update();
        assert_eq!(update["state"], "problem");
        assert_eq!(update["problem"], "микрофон отключён");
        assert_eq!(update["status"]["mic"]["level"], "bad");
        model.indicator = Indicator::Idle;
        model.mic = Some("Микрофон (ME6S)".into());
        let update = model.state_update();
        assert_eq!(update["status"]["mic"]["text"], "Микрофон (ME6S)");
        assert!(update["problem"].is_null());
    }

    #[test]
    fn level_follows_voice_and_ignores_mic_when_idle() {
        let mut model = WindowModel {
            indicator: Indicator::Idle,
            ..WindowModel::default()
        };
        let t = Instant::now();
        // Ждёт «Джарвис»: микрофон не в счёт.
        assert!(model.level_due(t, 0.9, 0.0).is_none());
        // Джарвис говорит.
        let t = t + LEVEL_EVERY;
        assert_eq!(model.level_due(t, 0.0, 0.5).unwrap()["level"], 0.5);
        // Слишком рано — пропуск.
        assert!(model.level_due(t, 0.0, 0.1).is_none());
        // Слушает команду: микрофон в счёт.
        model.indicator = Indicator::Listening;
        let t = t + LEVEL_EVERY;
        assert_eq!(model.level_due(t, 0.8, 0.0).unwrap()["level"], 0.8);
        // Почти та же громкость — не отправляется.
        let t = t + LEVEL_EVERY;
        assert!(model.level_due(t, 0.81, 0.0).is_none());
    }

    #[test]
    fn commands_list_shows_permissions_and_locks_always_ask() {
        use jarvis_core::commands::{ActionSpec, Command, ReplyChoice};
        use jarvis_core::permissions::Answer;
        use jarvis_core::state::Reply;

        let command = |id: &str, pack: &str, always_ask: bool| Command {
            id: id.into(),
            pack: pack.into(),
            phrases: vec![format!("открой {id}")],
            description: format!("Открыть {id}"),
            reply: ReplyChoice::Reply(Reply::Done),
            always_ask,
            slots: Default::default(),
            action: ActionSpec::OpenUrl {
                url: format!("https://{id}.com"),
                find_tab: Vec::new(),
            },
            dir: "commands/test".into(),
        };
        let registry = Registry {
            commands: vec![
                command("youtube", "browser", false),
                command("lock", "system", true),
                command("google", "browser", false),
                command("music", "apps", false),
            ],
            ..Registry::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let mut permissions = PermissionStore::load(&dir.path().join("permissions.toml")).unwrap();
        permissions
            .record(&registry.commands[0], Answer::Always)
            .unwrap();
        permissions
            .record(&registry.commands[3], Answer::Never)
            .unwrap();

        let list = commands_json(&registry, &permissions);
        let list = list["commands"].as_array().unwrap();
        let ids: Vec<_> = list.iter().map(|c| c["id"].as_str().unwrap()).collect();
        // По пакетам, внутри пакета — как в манифесте.
        assert_eq!(ids, ["music", "youtube", "google", "lock"]);
        assert_eq!(list[0]["permission"], "never");
        assert_eq!(list[1]["permission"], "always");
        assert_eq!(list[2]["permission"], "ask");
        assert_eq!(list[2]["always_ask"], false);
        assert_eq!(list[3]["always_ask"], true);
        assert_eq!(list[1]["action"], "Открыть youtube");
    }

    #[test]
    fn page_is_offline_only() {
        // Страница не тянет ничего из сети и не вставляет данные как HTML.
        assert!(PAGE.contains("default-src 'none'"));
        for forbidden in [
            "<link",
            "@import",
            "src=\"http",
            "fetch(",
            "XMLHttpRequest",
            "innerHTML",
            "eval(",
        ] {
            assert!(!PAGE.contains(forbidden), "в странице: {forbidden}");
        }
    }
}
