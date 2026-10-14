//! Редактор команд главного окна (этап 9): простые команды личного пакета
//! `commands/my`.
//!
//! - Правится только личный пакет: стандартные перезаписываются обновлением
//!   программы.
//! - Только простые действия (сайт, программа, клавиши, мультимедиа, фраза):
//!   цепочки, вопросы, скрипты и параметры из речи показываются без правки —
//!   редактор их не испортит.
//! - Файл правится через `toml_edit` (комментарии и остальные команды
//!   остаются), перед записью весь набор команд проверяется тем же
//!   загрузчиком ([`Registry::load_with`]): ошибка — файл не меняется.
//! - Изменённое действие меняет отпечаток команды — Джарвис снова спросит
//!   разрешение (раздел 2 ТЗ).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jarvis_core::commands::{ActionSpec, Command, MANIFEST_FILE, MediaOp, Registry, ReplyChoice};
use serde::{Deserialize, Serialize};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

/// Папка личного пакета внутри папки команд.
pub const USER_PACK: &str = "my";
const MAX_PHRASES: usize = 20;
const MAX_TEXT: usize = 300;

const NEW_MANIFEST: &str = "# Ваши команды. Редактор команд в окне Джарвиса пишет сюда; можно править\n\
# и вручную (формат — README, «Как добавить свою команду»).\n\
pack = \"my\"\n\
version = 1\n";

/// Действие, которое умеет редактор.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SimpleAction {
    OpenUrl {
        url: String,
        /// Заголовки вкладок для поиска уже открытой («* - YouTube»).
        #[serde(default)]
        find_tab: Vec<String>,
    },
    LaunchApp {
        path: String,
        #[serde(default)]
        args: Vec<String>,
        /// Уже запущена — вывести окно вперёд вместо нового запуска.
        #[serde(default)]
        focus: bool,
        /// Добавить команду закрытия («закрой …», `close = true` в манифесте).
        #[serde(default)]
        close: bool,
    },
    Hotkey {
        keys: Vec<String>,
    },
    KeyPress {
        key: String,
    },
    Media {
        operation: MediaOp,
        /// Громкость 0–100 для `volume_set`.
        #[serde(default)]
        level: Option<u32>,
    },
    Say {
        reply: String,
    },
}

/// Команда в редакторе.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandForm {
    /// `None` — новая команда (id придумывается по первой фразе).
    #[serde(default)]
    pub id: Option<String>,
    pub phrases: Vec<String>,
    pub description: String,
    #[serde(default)]
    pub always_ask: bool,
    /// `ok` — «Запрос выполнен, сэр», `none` — молча.
    pub reply: String,
    pub action: SimpleAction,
}

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("{0}")]
    Form(String),
    #[error("команды `{0}` нет в личном пакете")]
    NotFound(String),
    #[error("не удалось прочитать {path}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("в {path} ошибка — исправьте её вручную")]
    Parse {
        path: String,
        source: toml_edit::TomlError,
    },
    #[error("команда не прошла проверку: {0}")]
    Invalid(String),
    #[error("не удалось записать {path}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

/// Итог сохранения: id и предупреждения (совпадающие фразы и т. п.).
#[derive(Debug)]
pub struct Saved {
    pub id: String,
    pub warnings: Vec<String>,
}

/// Команда в виде формы редактора, если её можно править в окне. Реестр —
/// чтобы узнать, есть ли у команды парная команда закрытия (`<id>_close`).
pub fn form_of(command: &Command, registry: &Registry) -> Option<CommandForm> {
    if command.pack != USER_PACK || !command.slots.is_empty() {
        return None;
    }
    let reply = match command.reply {
        ReplyChoice::Reply(r) => r.key().to_owned(),
        ReplyChoice::Silent => "none".to_owned(),
    };
    if reply != "ok" && reply != "none" {
        return None;
    }
    let action = match &command.action {
        ActionSpec::OpenUrl { url, find_tab } => SimpleAction::OpenUrl {
            url: url.clone(),
            find_tab: find_tab.clone(),
        },
        ActionSpec::LaunchApp {
            path,
            args,
            working_dir: None,
            focus,
        } => {
            // Своё имя процесса (не файл программы) редактор не сохранил бы.
            if focus
                .as_deref()
                .is_some_and(|f| !f.eq_ignore_ascii_case(&exe_name(path)))
            {
                return None;
            }
            let close_id = format!("{}_close", command.id);
            SimpleAction::LaunchApp {
                path: path.clone(),
                args: args.clone(),
                focus: focus.is_some(),
                close: registry
                    .get(&close_id)
                    .is_some_and(|c| matches!(c.action, ActionSpec::CloseApp { .. })),
            }
        }
        ActionSpec::Hotkey {
            keys,
            hold_ms: None,
        } => SimpleAction::Hotkey { keys: keys.clone() },
        ActionSpec::KeyPress {
            key,
            hold_ms: None,
            repeat: None,
            interval_ms: None,
        } => SimpleAction::KeyPress { key: key.clone() },
        ActionSpec::Media {
            operation,
            steps: None,
            level,
        } => {
            let level = match level {
                None => None,
                Some(text) => Some(text.trim().parse().ok()?),
            };
            SimpleAction::Media {
                operation: *operation,
                level,
            }
        }
        ActionSpec::Say {
            reply: Some(r),
            file: None,
        } => SimpleAction::Say { reply: r.clone() },
        _ => return None,
    };
    Some(CommandForm {
        id: Some(command.id.clone()),
        phrases: command.phrases.clone(),
        description: command.description.clone(),
        always_ask: command.always_ask,
        reply,
        action,
    })
}

/// Путь манифеста личного пакета.
pub fn user_manifest(commands_dir: &Path) -> PathBuf {
    commands_dir.join(USER_PACK).join(MANIFEST_FILE)
}

/// Сохранить команду (новую или изменённую) в личный пакет.
pub fn save(commands_dir: &Path, form: &CommandForm) -> Result<Saved, EditorError> {
    let form = clean(form)?;
    let path = user_manifest(commands_dir);
    let text = read_manifest(&path)?;
    let before = Registry::load(commands_dir);
    let id = match &form.id {
        Some(id) => id.clone(),
        None => {
            let taken: HashSet<String> = before.commands.iter().map(|c| c.id.clone()).collect();
            new_id(&form.phrases[0], &taken)
        }
    };
    let new_text = upsert(&text, &path, &id, &form)?;
    let warnings = check_and_write(commands_dir, &path, &before, &new_text, Some(&id))?;
    Ok(Saved { id, warnings })
}

/// Удалить команду из личного пакета.
pub fn delete(commands_dir: &Path, id: &str) -> Result<(), EditorError> {
    let path = user_manifest(commands_dir);
    let text = read_manifest(&path)?;
    let mut doc = parse(&text, &path)?;
    let commands = doc
        .get_mut("command")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| EditorError::NotFound(id.to_owned()))?;
    let index = commands
        .iter()
        .position(|t| table_id(t) == Some(id))
        .ok_or_else(|| EditorError::NotFound(id.to_owned()))?;
    commands.remove(index);
    let before = Registry::load(commands_dir);
    check_and_write(commands_dir, &path, &before, &doc.to_string(), None)?;
    Ok(())
}

/// Проверить форму и убрать лишние пробелы.
fn clean(form: &CommandForm) -> Result<CommandForm, EditorError> {
    let bad = |text: &str| Err(EditorError::Form(text.to_owned()));
    let phrases: Vec<String> = form
        .phrases
        .iter()
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect();
    if phrases.is_empty() {
        return bad("нужна хотя бы одна фраза");
    }
    if phrases.len() > MAX_PHRASES {
        return bad("слишком много фраз (не больше 20)");
    }
    if phrases
        .iter()
        .any(|p| p.chars().any(|c| c == '{' || c == '}'))
    {
        return bad("фигурные скобки — параметры из речи, их редактор не умеет");
    }
    let description = form.description.trim().to_owned();
    if description.is_empty() {
        return bad("опишите, что сделает команда (это звучит в вопросе-разрешении)");
    }
    if std::iter::once(&description)
        .chain(&phrases)
        .any(|t| t.chars().count() > MAX_TEXT)
    {
        return bad("слишком длинный текст");
    }
    if form.reply != "ok" && form.reply != "none" {
        return bad("ответ — «ok» или «none»");
    }
    let action = match &form.action {
        SimpleAction::OpenUrl { url, find_tab } => {
            let url = url.trim().to_owned();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return bad("адрес сайта начинается с https:// или http://");
            }
            SimpleAction::OpenUrl {
                url,
                find_tab: trimmed(find_tab),
            }
        }
        SimpleAction::LaunchApp {
            path,
            args,
            focus,
            close,
        } => SimpleAction::LaunchApp {
            path: path.trim().trim_matches('"').to_owned(),
            args: trimmed(args),
            focus: *focus,
            close: *close,
        },
        SimpleAction::Hotkey { keys } => {
            let keys = trimmed(keys);
            if keys.len() < 2 {
                return bad("сочетание — две клавиши и больше (одна — «Нажать клавишу»)");
            }
            SimpleAction::Hotkey { keys }
        }
        SimpleAction::KeyPress { key } => SimpleAction::KeyPress {
            key: key.trim().to_owned(),
        },
        SimpleAction::Media { operation, level } => {
            let level = match (operation, level) {
                (MediaOp::VolumeSet, Some(l)) if *l <= 100 => Some(*l),
                (MediaOp::VolumeSet, _) => return bad("громкость — число от 0 до 100"),
                _ => None,
            };
            SimpleAction::Media {
                operation: *operation,
                level,
            }
        }
        SimpleAction::Say { reply } => SimpleAction::Say {
            reply: reply.trim().to_owned(),
        },
    };
    Ok(CommandForm {
        id: form.id.clone(),
        phrases,
        description,
        always_ask: form.always_ask,
        reply: form.reply.clone(),
        action,
    })
}

fn trimmed(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn read_manifest(path: &Path) -> Result<String, EditorError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NEW_MANIFEST.to_owned()),
        Err(source) => Err(EditorError::Read {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn parse(text: &str, path: &Path) -> Result<DocumentMut, EditorError> {
    text.parse().map_err(|source| EditorError::Parse {
        path: path.display().to_string(),
        source,
    })
}

fn table_id(table: &Table) -> Option<&str> {
    table.get("id").and_then(Item::as_str)
}

/// Новый текст манифеста: команда `id` заменена или добавлена в конец.
fn upsert(text: &str, path: &Path, id: &str, form: &CommandForm) -> Result<String, EditorError> {
    let mut doc = parse(text, path)?;
    let commands = doc
        .as_table_mut()
        .entry("command")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| EditorError::Invalid("command в файле — не список команд".into()))?;
    let found = commands.iter().position(|t| table_id(t) == Some(id));
    let table = match found {
        Some(index) => commands
            .get_mut(index)
            .ok_or_else(|| EditorError::NotFound(id.to_owned()))?,
        None if form.id.is_some() => return Err(EditorError::NotFound(id.to_owned())),
        None => {
            commands.push(Table::new());
            let last = commands.len() - 1;
            commands
                .get_mut(last)
                .ok_or_else(|| EditorError::NotFound(id.to_owned()))?
        }
    };
    table.insert("id", value(id));
    table.insert("phrases", value(Array::from_iter(form.phrases.iter())));
    table.insert("description", value(form.description.as_str()));
    table.insert("reply", value(form.reply.as_str()));
    if form.always_ask {
        table.insert("always_ask", value(true));
    } else {
        table.remove("always_ask");
    }
    table.insert("action", Item::Table(action_table(&form.action)));
    // Закрытие — поле команды, не действия; свои фразы и процесс закрытия,
    // заданные вручную, редактор не трогает (пока закрытие включено).
    match &form.action {
        SimpleAction::LaunchApp { close: true, .. } => {
            table.insert("close", value(true));
        }
        _ => {
            for key in ["close", "close_phrases", "close_process"] {
                table.remove(key);
            }
        }
    }
    Ok(doc.to_string())
}

fn action_table(action: &SimpleAction) -> Table {
    let mut t = Table::new();
    match action {
        SimpleAction::OpenUrl { url, find_tab } => {
            t.insert("type", value("open_url"));
            t.insert("url", value(url.as_str()));
            if !find_tab.is_empty() {
                t.insert("find_tab", value(Array::from_iter(find_tab.iter())));
            }
        }
        SimpleAction::LaunchApp {
            path, args, focus, ..
        } => {
            t.insert("type", value("launch_app"));
            t.insert("path", value(path.as_str()));
            if !args.is_empty() {
                t.insert("args", value(Array::from_iter(args.iter())));
            }
            if *focus {
                t.insert("focus", value(exe_name(path)));
            }
        }
        SimpleAction::Hotkey { keys } => {
            t.insert("type", value("hotkey"));
            t.insert("keys", value(Array::from_iter(keys.iter())));
        }
        SimpleAction::KeyPress { key } => {
            t.insert("type", value("key_press"));
            t.insert("key", value(key.as_str()));
        }
        SimpleAction::Media { operation, level } => {
            t.insert("type", value("media"));
            t.insert("operation", value(media_name(*operation)));
            if let Some(level) = level {
                t.insert("level", value(level.to_string()));
            }
        }
        SimpleAction::Say { reply } => {
            t.insert("type", value("say"));
            t.insert("reply", value(reply.as_str()));
        }
    }
    t
}

fn media_name(op: MediaOp) -> &'static str {
    match op {
        MediaOp::PlayPause => "play_pause",
        MediaOp::Next => "next",
        MediaOp::Previous => "previous",
        MediaOp::Stop => "stop",
        MediaOp::VolumeUp => "volume_up",
        MediaOp::VolumeDown => "volume_down",
        MediaOp::VolumeSet => "volume_set",
        MediaOp::Mute => "mute",
    }
}

/// Имя файла программы для `focus` («C:\…\Telegram.exe» → «Telegram.exe»).
fn exe_name(path: &str) -> String {
    Path::new(path.trim())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Проверить новый текст всем загрузчиком; новые ошибки личного пакета —
/// отказ; иначе записать. Вернуть новые предупреждения.
fn check_and_write(
    commands_dir: &Path,
    path: &Path,
    before: &Registry,
    new_text: &str,
    must_have: Option<&str>,
) -> Result<Vec<String>, EditorError> {
    let after = Registry::load_with(commands_dir, Some((path, new_text)));
    let old_errors: HashSet<String> = before.errors.iter().map(|e| e.message.clone()).collect();
    if let Some(problem) = after
        .errors
        .iter()
        .find(|e| e.file == path && !old_errors.contains(&e.message))
    {
        return Err(EditorError::Invalid(problem.message.clone()));
    }
    if let Some(id) = must_have
        && after.get(id).is_none()
    {
        return Err(EditorError::Invalid(format!(
            "команда `{id}` не загрузилась"
        )));
    }
    let old_warnings: HashSet<&String> = before.warnings.iter().collect();
    let warnings = after
        .warnings
        .iter()
        .filter(|w| !old_warnings.contains(w))
        .cloned()
        .collect();

    let write_err = |source| EditorError::Write {
        path: path.display().to_string(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(write_err)?;
    }
    // Через временный файл: оборванная запись не портит команды.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, new_text).map_err(write_err)?;
    std::fs::rename(&tmp, path).map_err(write_err)?;
    Ok(warnings)
}

/// id по первой фразе латиницей: «открой телеграм» → `my_otkroy_telegram`.
fn new_id(phrase: &str, taken: &HashSet<String>) -> String {
    let mut slug = String::new();
    for c in phrase.to_lowercase().chars() {
        let latin = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' | 'э' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "sch",
            'ы' => "y",
            'ю' => "yu",
            'я' => "ya",
            'ъ' | 'ь' => "",
            c if c.is_ascii_alphanumeric() => {
                slug.push(c);
                continue;
            }
            _ => "_",
        };
        slug.push_str(latin);
    }
    let slug: Vec<&str> = slug.split('_').filter(|s| !s.is_empty()).collect();
    let base = format!("{USER_PACK}_{}", slug.join("_"));
    let base = base
        .trim_end_matches('_')
        .chars()
        .take(48)
        .collect::<String>();
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !taken.contains(id))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(phrase: &str, action: SimpleAction) -> CommandForm {
        CommandForm {
            id: None,
            phrases: vec![phrase.into()],
            description: "откроет что-то".into(),
            always_ask: false,
            reply: "ok".into(),
            action,
        }
    }

    fn site(url: &str) -> SimpleAction {
        SimpleAction::OpenUrl {
            url: url.into(),
            find_tab: Vec::new(),
        }
    }

    /// Папка команд с личным пакетом (комментарий и одна команда) и чужим.
    fn commands_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("my")).unwrap();
        std::fs::write(
            dir.path().join("my/command.toml"),
            "# мой комментарий\npack = \"my\"\nversion = 1\n\n[[command]]\nid = \"my_keep\"\nphrases = [\"открой кип\"]\ndescription = \"откроет Keep\"\n[command.action]\ntype = \"open_url\"\nurl = \"https://keep.google.com\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("browser")).unwrap();
        std::fs::write(
            dir.path().join("browser/command.toml"),
            "pack = \"browser\"\nversion = 1\n\n[[command]]\nid = \"open_youtube\"\nphrases = [\"открой ютуб\"]\ndescription = \"откроет YouTube\"\n[command.action]\ntype = \"open_url\"\nurl = \"https://www.youtube.com\"\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn new_command_is_added_and_loads() {
        let dir = commands_dir();
        let saved = save(
            dir.path(),
            &form("открой почту", site("https://mail.google.com")),
        )
        .unwrap();
        assert_eq!(saved.id, "my_otkroy_pochtu");
        let text = std::fs::read_to_string(dir.path().join("my/command.toml")).unwrap();
        assert!(text.starts_with("# мой комментарий\n"), "{text}");
        let registry = Registry::load(dir.path());
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let cmd = registry.get("my_otkroy_pochtu").unwrap();
        assert_eq!(cmd.phrases, ["открой почту"]);
        assert!(registry.get("my_keep").is_some());
        // Форма из загруженной команды — та же.
        let back = form_of(cmd, &registry).unwrap();
        assert_eq!(back.action, site("https://mail.google.com"));
    }

    #[test]
    fn edit_replaces_in_place_and_delete_removes() {
        let dir = commands_dir();
        let mut f = form("открой заметки", site("https://keep.google.com/u/3"));
        f.id = Some("my_keep".into());
        f.always_ask = true;
        save(dir.path(), &f).unwrap();
        let registry = Registry::load(dir.path());
        let cmd = registry.get("my_keep").unwrap();
        assert_eq!(cmd.phrases, ["открой заметки"]);
        assert!(cmd.always_ask);
        delete(dir.path(), "my_keep").unwrap();
        assert!(Registry::load(dir.path()).get("my_keep").is_none());
        assert!(matches!(
            delete(dir.path(), "my_keep"),
            Err(EditorError::NotFound(_))
        ));
    }

    #[test]
    fn invalid_command_does_not_touch_file() {
        let dir = commands_dir();
        let path = dir.path().join("my/command.toml");
        let before = std::fs::read_to_string(&path).unwrap();
        // Нет такой программы — загрузчик отказывает.
        let err = save(
            dir.path(),
            &form(
                "запусти чудо",
                SimpleAction::LaunchApp {
                    path: r"C:\нет\такой\программы.exe".into(),
                    args: Vec::new(),
                    focus: true,
                    close: false,
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, EditorError::Invalid(_)), "{err:?}");
        // Неизвестная клавиша.
        let err = save(
            dir.path(),
            &form(
                "нажми чудо",
                SimpleAction::KeyPress {
                    key: "нетакой".into(),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, EditorError::Invalid(_)), "{err:?}");
        // Не сайт.
        let err = save(dir.path(), &form("открой файл", site("file:///C:/x"))).unwrap_err();
        assert!(matches!(err, EditorError::Form(_)));
        // Нет фраз.
        let mut f = form(" ", site("https://a.com"));
        f.phrases = vec!["  ".into()];
        assert!(matches!(save(dir.path(), &f), Err(EditorError::Form(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn missing_user_pack_is_created() {
        let dir = commands_dir();
        std::fs::remove_dir_all(dir.path().join("my")).unwrap();
        save(
            dir.path(),
            &form(
                "громкость пятьдесят",
                SimpleAction::Media {
                    operation: MediaOp::VolumeSet,
                    level: Some(50),
                },
            ),
        )
        .unwrap();
        let registry = Registry::load(dir.path());
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        assert_eq!(registry.get("my_gromkost_pyatdesyat").unwrap().pack, "my");
    }

    #[test]
    fn only_user_pack_simple_commands_are_editable() {
        let dir = commands_dir();
        let registry = Registry::load(dir.path());
        assert!(form_of(registry.get("my_keep").unwrap(), &registry).is_some());
        assert!(form_of(registry.get("open_youtube").unwrap(), &registry).is_none());
    }

    #[test]
    fn launch_app_with_close_adds_and_removes_close_command() {
        let dir = commands_dir();
        // Программа, которая точно есть: сам тестовый исполняемый файл.
        let exe = std::env::current_exe().unwrap().display().to_string();
        let app = |close: bool| SimpleAction::LaunchApp {
            path: exe.clone(),
            args: Vec::new(),
            focus: true,
            close,
        };
        let saved = save(dir.path(), &form("запусти тест", app(true))).unwrap();
        let registry = Registry::load(dir.path());
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let close = registry.get(&format!("{}_close", saved.id)).unwrap();
        assert_eq!(close.phrases, ["закрой тест"]);
        let back = form_of(registry.get(&saved.id).unwrap(), &registry).unwrap();
        assert_eq!(back.action, app(true));
        // Закрытие сгенерировано — в редакторе его не правят.
        assert!(form_of(close, &registry).is_none());

        let mut f = form("запусти тест", app(false));
        f.id = Some(saved.id.clone());
        save(dir.path(), &f).unwrap();
        let registry = Registry::load(dir.path());
        assert!(registry.get(&format!("{}_close", saved.id)).is_none());
        let text = std::fs::read_to_string(dir.path().join("my/command.toml")).unwrap();
        assert!(!text.contains("close"), "{text}");
    }

    #[test]
    fn ids_are_latin_and_unique() {
        let taken: HashSet<String> = ["my_otkroy_telegram".to_owned()].into();
        assert_eq!(
            new_id("Открой Телеграм", &HashSet::new()),
            "my_otkroy_telegram"
        );
        assert_eq!(new_id("открой телеграм", &taken), "my_otkroy_telegram_2");
        assert_eq!(new_id("запусти CS 2!", &HashSet::new()), "my_zapusti_cs_2");
    }
}
