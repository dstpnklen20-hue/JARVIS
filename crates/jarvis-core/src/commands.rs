//! Пакеты команд (раздел 5 ТЗ): `commands/<пакет>/command.toml`.
//!
//! - Поля проверяются строго (`deny_unknown_fields`): опечатка — ошибка.
//! - Невалидный манифест пропускается с понятной ошибкой (файл, строка,
//!   причина); остальные пакеты работают.
//! - Дубли `id` между пакетами — второй пропускается; одинаковые фразы у
//!   разных команд — предупреждение.
//! - Разрешения здесь не хранятся и храниться не могут (раздел 2.2): в
//!   манифесте есть только пометка автора `always_ask`.

use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::state::Reply;

pub const MANIFEST_FILE: &str = "command.toml";
pub const MANIFEST_VERSION: u32 = 1;
pub const MAX_VOLUME_STEPS: u32 = 50;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    pack: String,
    version: u32,
    #[serde(rename = "command", default)]
    commands: Vec<CommandFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandFile {
    id: String,
    phrases: Vec<String>,
    /// Читается в вопросе-разрешении: «команда X <description>».
    description: String,
    #[serde(default = "default_reply")]
    reply: String,
    #[serde(default)]
    always_ask: bool,
    action: ActionSpec,
}

fn default_reply() -> String {
    Reply::Done.key().to_owned()
}

/// Действие команды (раздел 6 ТЗ). Этап 2: `say`, `open_url`, `media`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionSpec {
    /// Проиграть ответ из звукового пакета или свой WAV из папки пакета команд.
    Say {
        reply: Option<String>,
        file: Option<PathBuf>,
    },
    /// Открыть адрес в браузере по умолчанию.
    OpenUrl { url: String },
    /// Мультимедиа-клавиши: пауза, треки, громкость.
    Media {
        operation: MediaOp,
        /// Сколько нажатий для громкости (одно ≈ 2 %).
        steps: Option<u32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOp {
    PlayPause,
    Next,
    Previous,
    Stop,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl MediaOp {
    pub fn is_volume(self) -> bool {
        matches!(self, MediaOp::VolumeUp | MediaOp::VolumeDown)
    }
}

impl ActionSpec {
    /// Что произойдёт — для журнала и вопроса-разрешения.
    pub fn summary(&self) -> String {
        match self {
            ActionSpec::Say { reply: Some(r), .. } => format!("скажет ответ «{r}»"),
            ActionSpec::Say { file: Some(f), .. } => format!("проиграет {}", f.display()),
            ActionSpec::Say { .. } => "ничего не скажет".into(),
            ActionSpec::OpenUrl { url } => format!("откроет {url}"),
            ActionSpec::Media { operation, steps } => match steps {
                Some(n) => format!("нажмёт {operation:?} ×{n}"),
                None => format!("нажмёт {operation:?}"),
            },
        }
    }
}

/// Что сказать после успешного действия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyChoice {
    Reply(Reply),
    Silent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub id: String,
    pub pack: String,
    pub phrases: Vec<String>,
    pub description: String,
    pub reply: ReplyChoice,
    pub always_ask: bool,
    pub action: ActionSpec,
    /// Папка пакета — от неё считаются пути в действиях.
    pub dir: PathBuf,
}

/// Ошибка в одном манифесте или команде.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadProblem {
    pub file: PathBuf,
    pub message: String,
}

impl fmt::Display for LoadProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.file.display(), self.message)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub commands: Vec<Command>,
    /// Пропущенные манифесты и команды.
    pub errors: Vec<LoadProblem>,
    /// Загружено, но стоит посмотреть (конфликтующие фразы).
    pub warnings: Vec<String>,
}

impl Registry {
    /// Загружает все пакеты из `dir` (каждая подпапка с `command.toml`).
    pub fn load(dir: &Path) -> Self {
        let mut registry = Registry::default();
        let mut manifests: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok().map(|e| e.path().join(MANIFEST_FILE)))
                .filter(|p| p.is_file())
                .collect(),
            Err(e) => {
                registry.errors.push(LoadProblem {
                    file: dir.to_path_buf(),
                    message: format!("папка команд недоступна: {e}"),
                });
                return registry;
            }
        };
        manifests.sort();
        for path in manifests {
            match std::fs::read_to_string(&path) {
                Ok(text) => registry.add_manifest(&path, &text),
                Err(e) => registry.errors.push(LoadProblem {
                    file: path,
                    message: format!("не удалось прочитать: {e}"),
                }),
            }
        }
        registry.find_conflicts();
        registry
    }

    /// Добавляет один манифест (путь — для сообщений и относительных путей).
    pub fn add_manifest(&mut self, path: &Path, text: &str) {
        let problem = |message: String| LoadProblem {
            file: path.to_path_buf(),
            message,
        };
        let file: ManifestFile = match toml::from_str(text) {
            Ok(f) => f,
            Err(e) => {
                self.errors
                    .push(problem(e.to_string().trim_end().to_owned()));
                return;
            }
        };
        if file.version != MANIFEST_VERSION {
            self.errors.push(problem(format!(
                "version = {} не поддерживается (нужна {MANIFEST_VERSION})",
                file.version
            )));
            return;
        }
        if file.pack.trim().is_empty() {
            self.errors
                .push(problem("pack не может быть пустым".into()));
            return;
        }
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut ids: HashSet<String> = self.commands.iter().map(|c| c.id.clone()).collect();
        for cmd in file.commands {
            match validate(cmd, &file.pack, &dir) {
                Ok(command) if ids.contains(&command.id) => self.errors.push(problem(format!(
                    "команда `{}` пропущена: такой id уже есть в другом пакете",
                    command.id
                ))),
                Ok(command) => {
                    ids.insert(command.id.clone());
                    self.commands.push(command);
                }
                Err(message) => self.errors.push(problem(message)),
            }
        }
    }

    fn find_conflicts(&mut self) {
        let phrases: Vec<Vec<&str>> = self
            .commands
            .iter()
            .map(|c| c.phrases.iter().map(String::as_str).collect())
            .collect();
        let matcher = jarvis_nlu::CommandMatcher::new(&phrases, jarvis_nlu::Thresholds::default());
        for (a, b, phrase) in matcher.conflicts() {
            self.warnings.push(format!(
                "фраза «{phrase}» есть и у `{}`, и у `{}` — сработает первая",
                self.commands[a].id, self.commands[b].id
            ));
        }
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }
}

fn validate(cmd: CommandFile, pack: &str, dir: &Path) -> Result<Command, String> {
    let id = cmd.id.trim().to_owned();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "id `{}`: только латиница, цифры, «_» и «-»",
            cmd.id
        ));
    }
    let err = |message: String| Err(format!("команда `{id}`: {message}"));

    let phrases: Vec<String> = cmd
        .phrases
        .iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if phrases.is_empty() {
        return err("нужна хотя бы одна фраза".into());
    }
    if let Some(bad) = phrases.iter().find(|p| jarvis_nlu::normalize(p).is_empty()) {
        return err(format!("фраза «{bad}» после нормализации пустая"));
    }
    if cmd.description.trim().is_empty() {
        return err("description нужно для вопроса-разрешения".into());
    }

    let reply = match cmd.reply.as_str() {
        "none" => ReplyChoice::Silent,
        key => match Reply::ALL.into_iter().find(|r| r.key() == key) {
            Some(r) => ReplyChoice::Reply(r),
            None => {
                let mut known: Vec<&str> = Reply::ALL.iter().map(|r| r.key()).collect();
                known.push("none");
                return err(format!(
                    "reply = \"{key}\" неизвестен; допустимые: {}",
                    known.join(", ")
                ));
            }
        },
    };

    if let Err(message) = validate_action(&cmd.action, dir) {
        return err(message);
    }

    Ok(Command {
        id,
        pack: pack.to_owned(),
        phrases,
        description: cmd.description.trim().to_owned(),
        reply,
        always_ask: cmd.always_ask,
        action: cmd.action,
        dir: dir.to_path_buf(),
    })
}

fn validate_action(action: &ActionSpec, dir: &Path) -> Result<(), String> {
    match action {
        ActionSpec::Say { reply, file } => match (reply, file) {
            (Some(_), Some(_)) | (None, None) => {
                Err("say: укажите ровно одно из reply или file".into())
            }
            (Some(key), None) => Reply::ALL
                .iter()
                .any(|r| r.key() == key)
                .then_some(())
                .ok_or_else(|| format!("say: ответа «{key}» нет в звуковых пакетах")),
            (None, Some(file)) => {
                let path = inside_dir(dir, file)?;
                if !path.is_file() {
                    return Err(format!("say: файла {} нет", path.display()));
                }
                Ok(())
            }
        },
        ActionSpec::OpenUrl { url } => validate_url(url),
        ActionSpec::Media { operation, steps } => match (operation.is_volume(), steps) {
            (false, Some(_)) => Err("media: steps бывает только у volume_up/volume_down".into()),
            (true, Some(n)) if !(1..=MAX_VOLUME_STEPS).contains(n) => Err(format!(
                "media: steps = {n} вне диапазона 1–{MAX_VOLUME_STEPS}"
            )),
            _ => Ok(()),
        },
    }
}

/// Только http(s)-адреса: `file:`, `javascript:` и т. п. из манифеста не
/// запускаются — это делается явными типами действий с вопросом-разрешением.
pub fn validate_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or_else(|| format!("open_url: «{url}» — нужен адрес http:// или https://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() {
        return Err(format!("open_url: в «{url}» нет адреса сайта"));
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '"')
    {
        return Err(format!("open_url: в «{url}» недопустимые символы"));
    }
    Ok(())
}

/// Путь из манифеста — строго внутри папки пакета (без `..` и абсолютных путей).
fn inside_dir(dir: &Path, relative: &Path) -> Result<PathBuf, String> {
    let escapes = relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
    if escapes {
        return Err(format!(
            "путь {} должен быть внутри папки пакета",
            relative.display()
        ));
    }
    Ok(dir.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
pack = "browser"
version = 1

[[command]]
id = "open_youtube"
phrases = ["открой ютуб", "запусти ютуб"]
description = "откроет YouTube в браузере"
reply = "ok"
[command.action]
type = "open_url"
url = "https://www.youtube.com"

[[command]]
id = "next"
phrases = ["следующий трек"]
description = "переключит на следующий трек"
reply = "none"
[command.action]
type = "media"
operation = "next"
"#;

    fn load(text: &str) -> Registry {
        let mut r = Registry::default();
        r.add_manifest(Path::new("commands/test/command.toml"), text);
        r
    }

    #[test]
    fn valid_manifest_loads() {
        let r = load(GOOD);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.commands.len(), 2);
        let yt = r.get("open_youtube").unwrap();
        assert_eq!(yt.pack, "browser");
        assert_eq!(yt.reply, ReplyChoice::Reply(Reply::Done));
        assert!(!yt.always_ask);
        assert_eq!(
            yt.action,
            ActionSpec::OpenUrl {
                url: "https://www.youtube.com".into()
            }
        );
        assert_eq!(r.get("next").unwrap().reply, ReplyChoice::Silent);
    }

    #[test]
    fn reply_defaults_to_ok() {
        let r = load(&GOOD.replace("reply = \"ok\"\n", ""));
        assert_eq!(
            r.get("open_youtube").unwrap().reply,
            ReplyChoice::Reply(Reply::Done)
        );
    }

    #[test]
    fn unknown_field_reports_line() {
        let r = load(&GOOD.replace("reply = \"ok\"", "replay = \"ok\""));
        assert!(r.commands.is_empty());
        let msg = r.errors[0].to_string();
        assert!(msg.contains("replay") && msg.contains("line"), "{msg}");
        assert!(msg.contains("commands/test/command.toml"), "{msg}");
    }

    #[test]
    fn unknown_action_type_is_rejected() {
        let r = load(&GOOD.replace("type = \"media\"", "type = \"teleport\""));
        assert!(r.errors[0].message.contains("teleport"), "{:?}", r.errors);
    }

    #[test]
    fn invalid_command_is_skipped_others_stay() {
        // Одна команда невалидна (пустые фразы) — вторая загружается.
        let text = GOOD.replace("phrases = [\"следующий трек\"]", "phrases = [\"  \"]");
        let r = load(&text);
        assert_eq!(r.commands.len(), 1);
        assert!(r.errors[0].message.contains("next"), "{:?}", r.errors);
    }

    #[test]
    fn duplicate_ids_across_packs() {
        let mut r = Registry::default();
        r.add_manifest(Path::new("a/command.toml"), GOOD);
        r.add_manifest(
            Path::new("b/command.toml"),
            &GOOD.replace("pack = \"browser\"", "pack = \"other\""),
        );
        assert_eq!(r.commands.len(), 2);
        assert_eq!(r.errors.len(), 2);
        assert!(r.errors[0].message.contains("уже есть"));
    }

    #[test]
    fn conflicting_phrases_warn() {
        let text = GOOD.replace(
            "phrases = [\"следующий трек\"]",
            "phrases = [\"Открой, пожалуйста, ютуб\"]",
        );
        let mut r = load(&text);
        r.find_conflicts();
        assert_eq!(r.commands.len(), 2);
        assert!(r.warnings[0].contains("открой ютуб"), "{:?}", r.warnings);
    }

    #[test]
    fn wrong_version_and_empty_pack() {
        assert!(
            load(&GOOD.replace("version = 1", "version = 2")).errors[0]
                .message
                .contains("version")
        );
        assert!(
            load(&GOOD.replace("pack = \"browser\"", "pack = \"\"")).errors[0]
                .message
                .contains("pack")
        );
    }

    #[test]
    fn bad_reply_and_id() {
        let r = load(&GOOD.replace("reply = \"ok\"", "reply = \"yes\""));
        assert!(r.errors[0].message.contains("none"), "{:?}", r.errors);
        let r = load(&GOOD.replace("id = \"open_youtube\"", "id = \"открыть ютуб\""));
        assert!(r.errors[0].message.contains("латиница"), "{:?}", r.errors);
    }

    #[test]
    fn urls_are_restricted_to_http() {
        assert!(validate_url("https://www.youtube.com/watch?v=1").is_ok());
        assert!(validate_url("http://example.com").is_ok());
        for bad in [
            "file:///C:/Windows/system32/cmd.exe",
            "javascript:alert(1)",
            "https://",
            "www.youtube.com",
            "https://a.com/\" & calc",
            "https://a.com/ b",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn media_steps_validation() {
        let vol = GOOD.replace(
            "operation = \"next\"",
            "operation = \"volume_up\"\nsteps = 5",
        );
        assert!(load(&vol).errors.is_empty());
        let bad = GOOD.replace(
            "operation = \"next\"",
            "operation = \"volume_up\"\nsteps = 100",
        );
        assert!(load(&bad).errors[0].message.contains("steps"));
        let wrong = GOOD.replace("operation = \"next\"", "operation = \"next\"\nsteps = 2");
        assert!(load(&wrong).errors[0].message.contains("volume"));
    }

    #[test]
    fn say_needs_exactly_one_source_inside_pack() {
        let say = |action: &str| {
            GOOD.replace(
                "type = \"media\"\noperation = \"next\"",
                &format!("type = \"say\"\n{action}"),
            )
        };
        assert!(load(&say("reply = \"ok\"")).errors.is_empty());
        assert!(!load(&say("")).errors.is_empty());
        assert!(
            !load(&say("reply = \"ok\"\nfile = \"a.wav\""))
                .errors
                .is_empty()
        );
        let escape = load(&say("file = \"../../secret.wav\""));
        assert!(
            escape.errors[0].message.contains("внутри"),
            "{:?}",
            escape.errors
        );
        let missing = load(&say("file = \"нет.wav\""));
        assert!(
            missing.errors[0].message.contains("нет"),
            "{:?}",
            missing.errors
        );
    }

    #[test]
    fn shipped_commands_are_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../commands");
        let r = Registry::load(&dir);
        assert!(r.errors.is_empty(), "{:#?}", r.errors);
        assert!(r.warnings.is_empty(), "{:#?}", r.warnings);
        assert!(r.get("open_youtube").is_some());
        assert!(r.get("media_play_pause").is_some());
    }
}
