//! Модель разрешений (раздел 2 ТЗ): Джарвис спрашивает, пользователь решает.
//!
//! - Разрешения живут только в `config/permissions.toml` — файле пользователя.
//!   Файлы внутри пакетов команд не читаются вовсе, поэтому скачанный пакет
//!   не может выдать себе разрешение (раздел 2.2).
//! - Разрешение привязано к `id` команды и к отпечатку (SHA-256) её действия:
//!   поменяли URL или путь — Джарвис спросит снова.
//! - «Всегда» не предлагается командам с `always_ask = true`, если
//!   пользователь сам не снял эту пометку (`allow_always = true` в файле).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::commands::Command;

pub const PERMISSIONS_FILE: &str = "config/permissions.toml";
const VERSION: u32 = 1;

/// Ответ пользователя на вопрос-разрешение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// «Да» — один раз.
    Once,
    /// «Всегда» — запомнить.
    Always,
    /// «Нет» — не выполнять (и тишина 10 секунд).
    No,
    /// «Никогда» — не выполнять и запомнить запрет.
    Never,
}

impl Answer {
    pub fn allows(self) -> bool {
        matches!(self, Answer::Once | Answer::Always)
    }
}

/// Что делать с командой сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Разрешено «всегда» раньше.
    Allowed,
    /// Запрещено «никогда» раньше.
    Denied,
    /// Нужно спросить; `offer_always` — можно ли предложить «Всегда».
    Ask { offer_always: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Always,
    Never,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<Decision>,
    /// Отпечаток действия, на которое дано решение.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated: Option<String>,
    /// Пользователь снял пометку автора `always_ask` (раздел 2.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    allow_always: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionFile {
    version: u32,
    #[serde(default)]
    commands: BTreeMap<String, Entry>,
}

impl Default for PermissionFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            commands: BTreeMap::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("не удалось прочитать {}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ошибка в {}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{}: version = {version} не поддерживается", path.display())]
    Version { path: PathBuf, version: u32 },
    #[error("не удалось сохранить {}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("не удалось записать разрешения: {0}")]
    Serialize(#[from] toml::ser::Error),
}

pub struct PermissionStore {
    path: PathBuf,
    file: PermissionFile,
}

impl PermissionStore {
    /// Загружает файл разрешений; нет файла — нет разрешений.
    pub fn load(path: &Path) -> Result<Self, PermissionError> {
        let file = match std::fs::read_to_string(path) {
            Ok(text) => {
                let file: PermissionFile =
                    toml::from_str(&text).map_err(|source| PermissionError::Parse {
                        path: path.to_path_buf(),
                        source,
                    })?;
                if file.version != VERSION {
                    return Err(PermissionError::Version {
                        path: path.to_path_buf(),
                        version: file.version,
                    });
                }
                file
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PermissionFile::default(),
            Err(source) => {
                return Err(PermissionError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn verdict(&self, command: &Command) -> Verdict {
        let entry = self.file.commands.get(&command.id);
        let offer_always = !command.always_ask || entry.is_some_and(|e| e.allow_always);
        let current = fingerprint(command);
        match entry {
            Some(Entry {
                decision: Some(decision),
                action: Some(action),
                ..
            }) if *action == current => match decision {
                Decision::Never => Verdict::Denied,
                Decision::Always if offer_always => Verdict::Allowed,
                Decision::Always => Verdict::Ask { offer_always },
            },
            _ => Verdict::Ask { offer_always },
        }
    }

    /// Запоминает «всегда» и «никогда» (и сохраняет файл). «Да» и «нет» —
    /// разовые, не запоминаются. «Всегда» для команды, которой его нельзя
    /// предлагать, считается разовым «да».
    pub fn record(&mut self, command: &Command, answer: Answer) -> Result<(), PermissionError> {
        let decision = match answer {
            Answer::Always if self.verdict_offers_always(command) => Decision::Always,
            Answer::Never => Decision::Never,
            Answer::Once | Answer::No | Answer::Always => return Ok(()),
        };
        let entry = self.file.commands.entry(command.id.clone()).or_default();
        entry.decision = Some(decision);
        entry.action = Some(fingerprint(command));
        entry.updated = Some(now_text());
        self.save()
    }

    fn verdict_offers_always(&self, command: &Command) -> bool {
        matches!(
            self.verdict(command),
            Verdict::Ask { offer_always: true } | Verdict::Allowed
        )
    }

    /// «Забудь разрешения»: все «всегда» и «никогда» удаляются. Снятые
    /// пометки `allow_always` — настройка пользователя, они остаются.
    pub fn forget_all(&mut self) -> Result<usize, PermissionError> {
        let mut forgotten = 0;
        self.file.commands.retain(|_, entry| {
            if entry.decision.take().is_some() {
                forgotten += 1;
            }
            entry.action = None;
            entry.updated = None;
            entry.allow_always
        });
        self.save()?;
        Ok(forgotten)
    }

    /// Сколько решений запомнено.
    pub fn remembered(&self) -> usize {
        self.file
            .commands
            .values()
            .filter(|e| e.decision.is_some())
            .count()
    }

    fn save(&self) -> Result<(), PermissionError> {
        let body = toml::to_string_pretty(&self.file)?;
        let text = format!(
            "# Разрешения Джарвиса — ваш файл, в git не попадает.\n\
             # decision: always (не спрашивать) или never (не выполнять).\n\
             # action — отпечаток действия: изменится команда — Джарвис спросит снова.\n\
             # allow_always = true — снять пометку автора always_ask.\n\
             # Удалите запись или скажите «Джарвис, забудь разрешения», чтобы он спрашивал снова.\n\n{body}"
        );
        let write_err = |source| PermissionError::Write {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(write_err)?;
        }
        // Через временный файл: оборванная запись не испортит разрешения.
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(write_err)?;
        std::fs::rename(&tmp, &self.path).map_err(write_err)
    }
}

/// Отпечаток действия команды: пакет, id, действие и слоты в каноническом
/// виде плюс содержимое файлов из папки пакета (скриптов, звуков). Подменили
/// скрипт, не меняя манифест, — отпечаток другой, Джарвис спросит снова.
pub fn fingerprint(command: &Command) -> String {
    let action = serde_json::to_string(&command.action).unwrap_or_default();
    let slots = serde_json::to_string(&command.slots).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(format!("{}\n{}\n{action}\n{slots}", command.pack, command.id).as_bytes());
    for file in command.action.pack_files() {
        hasher.update(b"\nfile:");
        match std::fs::read(command.dir.join(file)) {
            Ok(bytes) => hasher.update(Sha256::digest(&bytes)),
            Err(_) => hasher.update(b"unreadable"),
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now_text() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(&format)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{ActionSpec, ReplyChoice};
    use crate::state::Reply;

    fn command(id: &str, url: &str, always_ask: bool) -> Command {
        Command {
            id: id.into(),
            pack: "test".into(),
            phrases: vec!["фраза".into()],
            description: "откроет сайт".into(),
            reply: ReplyChoice::Reply(Reply::Done),
            always_ask,
            slots: Default::default(),
            action: ActionSpec::OpenUrl {
                url: url.into(),
                find_tab: Vec::new(),
            },
            dir: PathBuf::from("commands/test"),
        }
    }

    fn store() -> (tempfile::TempDir, PermissionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = PermissionStore::load(&dir.path().join("permissions.toml")).unwrap();
        (dir, store)
    }

    const ASK: Verdict = Verdict::Ask { offer_always: true };

    #[test]
    fn first_run_asks() {
        let (_d, s) = store();
        assert_eq!(s.verdict(&command("yt", "https://y.com", false)), ASK);
    }

    #[test]
    fn yes_is_one_time() {
        let (_d, mut s) = store();
        let c = command("yt", "https://y.com", false);
        s.record(&c, Answer::Once).unwrap();
        assert_eq!(s.verdict(&c), ASK);
        assert!(!s.path().exists(), "разовое «да» не пишется в файл");
    }

    #[test]
    fn always_is_remembered_and_survives_reload() {
        let (_d, mut s) = store();
        let c = command("yt", "https://y.com", false);
        s.record(&c, Answer::Always).unwrap();
        assert_eq!(s.verdict(&c), Verdict::Allowed);
        let reloaded = PermissionStore::load(s.path()).unwrap();
        assert_eq!(reloaded.verdict(&c), Verdict::Allowed);
    }

    #[test]
    fn no_is_one_time_never_is_remembered() {
        let (_d, mut s) = store();
        let c = command("yt", "https://y.com", false);
        s.record(&c, Answer::No).unwrap();
        assert_eq!(s.verdict(&c), ASK);
        s.record(&c, Answer::Never).unwrap();
        assert_eq!(s.verdict(&c), Verdict::Denied);
        assert_eq!(
            PermissionStore::load(s.path()).unwrap().verdict(&c),
            Verdict::Denied
        );
    }

    #[test]
    fn changed_action_asks_again() {
        let (_d, mut s) = store();
        s.record(&command("yt", "https://y.com", false), Answer::Always)
            .unwrap();
        let changed = command("yt", "https://evil.example", false);
        assert_eq!(s.verdict(&changed), ASK);
        // И запрет тоже привязан к действию.
        s.record(&changed, Answer::Never).unwrap();
        assert_eq!(s.verdict(&command("yt", "https://y.com", false)), ASK);
    }

    #[test]
    fn always_ask_never_offers_or_saves_always() {
        let (_d, mut s) = store();
        let c = command("shutdown", "https://x.com", true);
        assert_eq!(
            s.verdict(&c),
            Verdict::Ask {
                offer_always: false
            }
        );
        s.record(&c, Answer::Always).unwrap();
        assert_eq!(
            s.verdict(&c),
            Verdict::Ask {
                offer_always: false
            }
        );
        assert!(!s.path().exists());
    }

    #[test]
    fn user_can_lift_always_ask() {
        let (d, _) = store();
        let path = d.path().join("permissions.toml");
        std::fs::write(
            &path,
            "version = 1\n[commands.shutdown]\nallow_always = true\n",
        )
        .unwrap();
        let mut s = PermissionStore::load(&path).unwrap();
        let c = command("shutdown", "https://x.com", true);
        assert_eq!(s.verdict(&c), ASK);
        s.record(&c, Answer::Always).unwrap();
        assert_eq!(s.verdict(&c), Verdict::Allowed);
    }

    #[test]
    fn forget_clears_decisions_keeps_user_overrides() {
        let (d, _) = store();
        let path = d.path().join("permissions.toml");
        std::fs::write(
            &path,
            "version = 1\n[commands.shutdown]\nallow_always = true\n",
        )
        .unwrap();
        let mut s = PermissionStore::load(&path).unwrap();
        let a = command("a", "https://a.com", false);
        let b = command("b", "https://b.com", false);
        s.record(&a, Answer::Always).unwrap();
        s.record(&b, Answer::Never).unwrap();
        assert_eq!(s.remembered(), 2);
        assert_eq!(s.forget_all().unwrap(), 2);
        assert_eq!(s.verdict(&a), ASK);
        assert_eq!(s.verdict(&b), ASK);
        let shutdown = command("shutdown", "https://x.com", true);
        assert_eq!(s.verdict(&shutdown), ASK, "allow_always сохранился");
    }

    #[test]
    fn foreign_pack_cannot_grant_itself() {
        // 1. Поле разрешения в манифесте — ошибка разбора (deny_unknown_fields).
        let mut registry = crate::commands::Registry::default();
        registry.add_manifest(
            Path::new("commands/evil/command.toml"),
            "pack = \"evil\"\nversion = 1\n[[command]]\nid = \"x\"\nphrases = [\"икс\"]\n\
             description = \"d\"\npermission = \"always\"\n[command.action]\ntype = \"open_url\"\nurl = \"https://e.com\"\n",
        );
        assert!(registry.commands.is_empty());
        assert!(
            registry.errors[0].message.contains("permission"),
            "{:?}",
            registry.errors
        );

        // 2. permissions.toml в папке пакета не читается: хранилище знает только свой путь.
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("commands").join("evil");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(
            pack_dir.join("permissions.toml"),
            "version = 1\n[commands.x]\ndecision = \"always\"\naction = \"any\"\n",
        )
        .unwrap();
        let s = PermissionStore::load(&dir.path().join("config").join("permissions.toml")).unwrap();
        let mut c = command("x", "https://e.com", false);
        c.dir = pack_dir;
        assert_eq!(s.verdict(&c), ASK);

        // 3. Даже скопированная запись не подходит без верного отпечатка действия.
        let path = dir.path().join("permissions.toml");
        std::fs::write(
            &path,
            "version = 1\n[commands.x]\ndecision = \"always\"\naction = \"any\"\n",
        )
        .unwrap();
        assert_eq!(PermissionStore::load(&path).unwrap().verdict(&c), ASK);
    }

    #[test]
    fn broken_file_is_an_error_not_silent_permission() {
        let (d, _) = store();
        let path = d.path().join("permissions.toml");
        std::fs::write(&path, "version = 1\n[commands.x]\ndecision = \"maybe\"\n").unwrap();
        assert!(matches!(
            PermissionStore::load(&path),
            Err(PermissionError::Parse { .. })
        ));
        std::fs::write(&path, "version = 9\n").unwrap();
        assert!(matches!(
            PermissionStore::load(&path),
            Err(PermissionError::Version { .. })
        ));
    }

    #[test]
    fn swapped_script_content_changes_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("backup.ps1"), "Write-Host ok").unwrap();
        let mut c = command("backup", "https://x.com", false);
        c.dir = dir.path().to_path_buf();
        c.action = ActionSpec::RunScript {
            path: "backup.ps1".into(),
            args: vec![],
            timeout_sec: None,
        };
        let before = fingerprint(&c);
        assert_eq!(before, fingerprint(&c));
        std::fs::write(dir.path().join("backup.ps1"), "Remove-Item C:\\ -Recurse").unwrap();
        assert_ne!(
            before,
            fingerprint(&c),
            "подменённый скрипт — новый отпечаток"
        );
    }

    #[test]
    fn fingerprint_depends_on_pack_id_and_action() {
        let a = command("yt", "https://y.com", false);
        let mut other_pack = a.clone();
        other_pack.pack = "other".into();
        let mut other_url = a.clone();
        other_url.action = ActionSpec::OpenUrl {
            url: "https://z.com".into(),
            find_tab: Vec::new(),
        };
        let fp = fingerprint(&a);
        assert_eq!(fp.len(), 64);
        assert_eq!(fp, fingerprint(&a.clone()));
        assert_ne!(fp, fingerprint(&other_pack));
        assert_ne!(fp, fingerprint(&other_url));
    }
}
