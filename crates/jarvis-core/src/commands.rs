//! Пакеты команд (раздел 5 ТЗ): `commands/<пакет>/command.toml`.
//!
//! - Поля проверяются строго (`deny_unknown_fields`): опечатка — ошибка.
//! - Невалидный манифест пропускается с понятной ошибкой (файл, строка,
//!   причина); остальные пакеты работают.
//! - Дубли `id` между пакетами — второй пропускается; одинаковые фразы у
//!   разных команд — предупреждение.
//! - Разрешения здесь не хранятся и храниться не могут (раздел 2.2): в
//!   манифесте есть только пометка автора `always_ask`.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::slots::{SlotSpec, placeholders};
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
    /// Параметры из речи: «громкость на {level}» (раздел 2.5).
    #[serde(default)]
    slots: BTreeMap<String, SlotSpec>,
    action: ActionSpec,
}

fn default_reply() -> String {
    Reply::Done.key().to_owned()
}

/// Действие команды (раздел 6 ТЗ).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionSpec {
    /// Проиграть ответ из звукового пакета или свой WAV из папки пакета команд.
    Say {
        reply: Option<String>,
        file: Option<PathBuf>,
    },
    /// Открыть адрес в браузере по умолчанию. С `find_tab` сначала ищется
    /// уже открытая вкладка с подходящим заголовком («* - YouTube»): нашлась —
    /// переключиться на неё, нет — открыть адрес.
    OpenUrl {
        url: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        find_tab: Vec<String>,
    },
    /// Мультимедиа-клавиши и громкость.
    Media {
        operation: MediaOp,
        /// Сколько нажатий для volume_up/volume_down (одно ≈ 2 %).
        steps: Option<u32>,
        /// Уровень для volume_set: 0–100 или слот («{level}»).
        level: Option<String>,
    },
    /// Запустить программу (не ждать её завершения).
    LaunchApp {
        /// Полный путь, путь внутри пакета или имя из PATH («notepad.exe»).
        path: String,
        #[serde(default)]
        args: Vec<String>,
        working_dir: Option<PathBuf>,
    },
    /// Запустить скрипт из папки пакета и дождаться его (с тайм-аутом).
    RunScript {
        /// Путь внутри папки пакета.
        path: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        /// Секунды (1–3600); по умолчанию 30.
        timeout_sec: Option<u64>,
    },
    /// Нажать клавишу (скан-коды — работает в играх).
    KeyPress {
        key: String,
        /// Сколько держать нажатой, мс (по умолчанию 30).
        hold_ms: Option<u64>,
        /// Сколько раз нажать (по умолчанию 1).
        repeat: Option<u32>,
        /// Пауза между нажатиями, мс (по умолчанию 50).
        interval_ms: Option<u64>,
    },
    /// Нажать сочетание: клавиши нажимаются по порядку, отпускаются в обратном.
    Hotkey {
        keys: Vec<String>,
        hold_ms: Option<u64>,
    },
    /// Питание: блокировка, сон, выход, перезагрузка, выключение.
    SystemPower { operation: PowerOp },
    /// Пауза (имеет смысл внутри `sequence`).
    Wait { ms: u64 },
    /// Цепочка действий по порядку; ошибка любого шага прерывает цепочку.
    Sequence { steps: Vec<ActionSpec> },
    /// Дождаться, пока запустится процесс (`faceitclient.exe`); не дождались
    /// за `timeout_sec` — ошибка («Не удалось, сэр»).
    WaitProcess { name: String, timeout_sec: u64 },
    /// Спросить голосом (WAV из папки пакета) и ждать ответа: ответ из
    /// `choices` выполняет команду с этим id — со своим разрешением.
    /// Только отдельным действием или последним шагом цепочки.
    Ask {
        file: PathBuf,
        /// Как отвечают → id команды.
        choices: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOp {
    PlayPause,
    Next,
    Previous,
    Stop,
    VolumeUp,
    VolumeDown,
    /// Установить громкость точно (`level`).
    VolumeSet,
    Mute,
}

impl MediaOp {
    pub fn is_volume(self) -> bool {
        matches!(self, MediaOp::VolumeUp | MediaOp::VolumeDown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerOp {
    Lock,
    Sleep,
    Logoff,
    Restart,
    Shutdown,
}

pub const DEFAULT_SCRIPT_TIMEOUT_SEC: u64 = 30;
pub const MAX_SCRIPT_TIMEOUT_SEC: u64 = 3600;
pub const MAX_WAIT_MS: u64 = 60_000;
pub const MAX_KEY_HOLD_MS: u64 = 10_000;
pub const MAX_KEY_REPEAT: u32 = 100;
pub const MAX_SEQUENCE_STEPS: usize = 50;
pub const MAX_PROCESS_WAIT_SEC: u64 = 300;
pub const MAX_ASK_CHOICES: usize = 30;
pub const MAX_TAB_PATTERNS: usize = 10;

impl ActionSpec {
    /// Что произойдёт — для журнала и вопроса-разрешения.
    pub fn summary(&self) -> String {
        match self {
            ActionSpec::Say { reply: Some(r), .. } => format!("скажет ответ «{r}»"),
            ActionSpec::Say { file: Some(f), .. } => format!("проиграет {}", f.display()),
            ActionSpec::Say { .. } => "ничего не скажет".into(),
            ActionSpec::OpenUrl { url, find_tab } if find_tab.is_empty() => {
                format!("откроет {url}")
            }
            ActionSpec::OpenUrl { url, find_tab } => format!(
                "переключится на открытую вкладку «{}» или откроет {url}",
                find_tab.join("» / «")
            ),
            ActionSpec::Media {
                operation: MediaOp::VolumeSet,
                level,
                ..
            } => format!("установит громкость {}%", level.as_deref().unwrap_or("?")),
            ActionSpec::Media {
                operation, steps, ..
            } => match steps {
                Some(n) => format!("нажмёт {operation:?} ×{n}"),
                None => format!("нажмёт {operation:?}"),
            },
            ActionSpec::LaunchApp { path, args, .. } => {
                format!("запустит {path}{}", args_text(args))
            }
            ActionSpec::RunScript {
                path,
                args,
                timeout_sec,
            } => format!(
                "выполнит скрипт {}{} (не дольше {} с)",
                path.display(),
                args_text(args),
                timeout_sec.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SEC)
            ),
            ActionSpec::KeyPress { key, repeat, .. } => match repeat {
                Some(n) if *n > 1 => format!("нажмёт клавишу {key} ×{n}"),
                _ => format!("нажмёт клавишу {key}"),
            },
            ActionSpec::Hotkey { keys, .. } => format!("нажмёт {}", keys.join("+")),
            ActionSpec::SystemPower { operation } => match operation {
                PowerOp::Lock => "заблокирует компьютер".into(),
                PowerOp::Sleep => "переведёт компьютер в сон".into(),
                PowerOp::Logoff => "выйдет из учётной записи".into(),
                PowerOp::Restart => "перезагрузит компьютер".into(),
                PowerOp::Shutdown => "выключит компьютер".into(),
            },
            ActionSpec::Wait { ms } => format!("подождёт {ms} мс"),
            ActionSpec::WaitProcess { name, timeout_sec } => {
                format!("дождётся запуска {name} (не дольше {timeout_sec} с)")
            }
            ActionSpec::Ask { file, choices } => format!(
                "спросит ({}) и по ответу выполнит: {}",
                file.display(),
                choices
                    .iter()
                    .map(|(answer, id)| format!("«{answer}» → {id}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ActionSpec::Sequence { steps } => steps
                .iter()
                .map(ActionSpec::summary)
                .collect::<Vec<_>>()
                .join(", затем "),
        }
    }

    /// Все строки действия, в которые можно подставлять слоты.
    fn slot_targets(&self) -> Vec<&str> {
        match self {
            ActionSpec::OpenUrl { url, .. } => vec![url.as_str()],
            ActionSpec::Media { level: Some(l), .. } => vec![l.as_str()],
            ActionSpec::LaunchApp { args, .. } | ActionSpec::RunScript { args, .. } => {
                args.iter().map(String::as_str).collect()
            }
            ActionSpec::Sequence { steps } => {
                steps.iter().flat_map(ActionSpec::slot_targets).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Пути к файлам внутри папки пакета (для отпечатка разрешения).
    pub fn pack_files(&self) -> Vec<&Path> {
        match self {
            ActionSpec::RunScript { path, .. } => vec![path.as_path()],
            ActionSpec::Say { file: Some(f), .. } | ActionSpec::Ask { file: f, .. } => {
                vec![f.as_path()]
            }
            ActionSpec::Sequence { steps } => {
                steps.iter().flat_map(ActionSpec::pack_files).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Команды, которые может выполнить ответ на вопрос (`ask`).
    pub fn ask_targets(&self) -> Vec<&str> {
        match self {
            ActionSpec::Ask { choices, .. } => choices.values().map(String::as_str).collect(),
            ActionSpec::Sequence { steps } => {
                steps.iter().flat_map(ActionSpec::ask_targets).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Действие с подставленными значениями слотов («{level}» → «40»).
    pub fn with_slots(&self, values: &crate::slots::SlotValues) -> ActionSpec {
        let fill = |s: &String| crate::slots::fill(s, values);
        match self {
            ActionSpec::OpenUrl { url, find_tab } => ActionSpec::OpenUrl {
                url: fill(url),
                find_tab: find_tab.clone(),
            },
            ActionSpec::Media {
                operation,
                steps,
                level,
            } => ActionSpec::Media {
                operation: *operation,
                steps: *steps,
                level: level.as_ref().map(fill),
            },
            ActionSpec::LaunchApp {
                path,
                args,
                working_dir,
            } => ActionSpec::LaunchApp {
                path: path.clone(),
                args: args.iter().map(fill).collect(),
                working_dir: working_dir.clone(),
            },
            ActionSpec::RunScript {
                path,
                args,
                timeout_sec,
            } => ActionSpec::RunScript {
                path: path.clone(),
                args: args.iter().map(fill).collect(),
                timeout_sec: *timeout_sec,
            },
            ActionSpec::Sequence { steps } => ActionSpec::Sequence {
                steps: steps.iter().map(|s| s.with_slots(values)).collect(),
            },
            other => other.clone(),
        }
    }
}

fn args_text(args: &[String]) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(" с аргументами {}", args.join(" "))
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
    /// Слоты: имя → тип и допустимые значения.
    pub slots: BTreeMap<String, SlotSpec>,
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
        registry.check_ask_targets();
        registry
    }

    /// Ответ на вопрос (`ask`) должен вести к существующей команде — иначе
    /// команда с вопросом пропускается.
    fn check_ask_targets(&mut self) {
        let ids: HashSet<String> = self.commands.iter().map(|c| c.id.clone()).collect();
        let mut broken = Vec::new();
        for command in &self.commands {
            if let Some(missing) = command
                .action
                .ask_targets()
                .into_iter()
                .find(|t| !ids.contains(*t))
            {
                broken.push((command.id.clone(), command.dir.clone(), missing.to_owned()));
            }
        }
        for (id, dir, missing) in broken {
            self.commands.retain(|c| c.id != id);
            self.errors.push(LoadProblem {
                file: dir.join(MANIFEST_FILE),
                message: format!(
                    "команда `{id}` пропущена: ответ ведёт к несуществующей команде `{missing}`"
                ),
            });
        }
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

    if let Err(message) = validate_slots(&cmd.slots, &phrases, &cmd.action) {
        return err(message);
    }
    if let Err(message) = validate_action(&cmd.action, dir, &cmd.slots, false) {
        return err(message);
    }

    Ok(Command {
        id,
        pack: pack.to_owned(),
        phrases,
        description: cmd.description.trim().to_owned(),
        reply,
        always_ask: cmd.always_ask,
        slots: cmd.slots,
        action: cmd.action,
        dir: dir.to_path_buf(),
    })
}

/// Слоты объявлены, использованы и упомянуты только там, где можно.
fn validate_slots(
    slots: &BTreeMap<String, SlotSpec>,
    phrases: &[String],
    action: &ActionSpec,
) -> Result<(), String> {
    for (name, spec) in slots {
        spec.validate(name)?;
        if !phrases
            .iter()
            .any(|p| placeholders(p).contains(&name.as_str()))
        {
            return Err(format!("слот `{name}` не встречается ни в одной фразе"));
        }
    }
    for phrase in phrases {
        if let Some(bad) = placeholders(phrase)
            .into_iter()
            .find(|p| !slots.contains_key(*p))
        {
            return Err(format!("во фразе «{phrase}» слот `{bad}` не объявлен"));
        }
    }
    if let Some(bad) = action
        .slot_targets()
        .into_iter()
        .flat_map(placeholders)
        .find(|p| !slots.contains_key(*p))
    {
        return Err(format!("в действии слот `{bad}` не объявлен"));
    }
    Ok(())
}

fn validate_action(
    action: &ActionSpec,
    dir: &Path,
    slots: &BTreeMap<String, SlotSpec>,
    in_sequence: bool,
) -> Result<(), String> {
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
        ActionSpec::OpenUrl { url, find_tab } => {
            validate_url(url)?;
            if find_tab.len() > MAX_TAB_PATTERNS {
                return Err(format!(
                    "open_url: в find_tab больше {MAX_TAB_PATTERNS} шаблонов"
                ));
            }
            if let Some(bad) = find_tab
                .iter()
                .find(|p| p.trim().chars().all(|c| c == '*' || c.is_whitespace()))
            {
                return Err(format!(
                    "open_url: шаблон вкладки «{bad}» подходит к любой вкладке"
                ));
            }
            Ok(())
        }
        ActionSpec::Media {
            operation,
            steps,
            level,
        } => {
            match (operation.is_volume(), steps) {
                (false, Some(_)) => {
                    return Err("media: steps бывает только у volume_up/volume_down".into());
                }
                (true, Some(n)) if !(1..=MAX_VOLUME_STEPS).contains(n) => {
                    return Err(format!(
                        "media: steps = {n} вне диапазона 1–{MAX_VOLUME_STEPS}"
                    ));
                }
                _ => {}
            }
            match (operation, level) {
                (MediaOp::VolumeSet, None) => Err("media: volume_set требует level".into()),
                (MediaOp::VolumeSet, Some(level)) => validate_level(level, slots),
                (_, Some(_)) => Err("media: level бывает только у volume_set".into()),
                (_, None) => Ok(()),
            }
        }
        ActionSpec::LaunchApp {
            path,
            args: _,
            working_dir,
        } => {
            no_slots_in_path(path)?;
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return Err("launch_app: пустой путь".into());
            }
            let as_path = Path::new(trimmed);
            let has_dir = trimmed.contains(['\\', '/']);
            if as_path.is_absolute() {
                if !as_path.is_file() {
                    return Err(format!("launch_app: программы {trimmed} нет"));
                }
            } else if has_dir {
                let full = inside_dir(dir, as_path)?;
                if !full.is_file() {
                    return Err(format!("launch_app: программы {} нет", full.display()));
                }
            }
            // Голое имя («notepad.exe») ищется в PATH при запуске.
            if let Some(wd) = working_dir {
                no_slots_in_path(&wd.to_string_lossy())?;
                if !wd.is_dir() {
                    return Err(format!("launch_app: папки {} нет", wd.display()));
                }
            }
            Ok(())
        }
        ActionSpec::RunScript {
            path,
            args,
            timeout_sec,
        } => {
            no_slots_in_path(&path.to_string_lossy())?;
            let full = inside_dir(dir, path)?;
            if !full.is_file() {
                return Err(format!("run_script: скрипта {} нет", full.display()));
            }
            let kind = ScriptKind::of(path).ok_or_else(|| {
                format!(
                    "run_script: {} — поддерживаются .exe, .bat, .cmd, .ps1, .py",
                    path.display()
                )
            })?;
            if let Some(t) = timeout_sec
                && !(1..=MAX_SCRIPT_TIMEOUT_SEC).contains(t)
            {
                return Err(format!(
                    "run_script: timeout_sec = {t} вне диапазона 1–{MAX_SCRIPT_TIMEOUT_SEC}"
                ));
            }
            if kind == ScriptKind::Batch
                && let Some(bad) = args.iter().find(|a| a.contains(BATCH_METACHARS))
            {
                // cmd.exe заново разбирает строку — спецсимволы стали бы командами.
                return Err(format!(
                    "run_script: в аргументе «{bad}» для .bat/.cmd недопустимы символы {BATCH_METACHARS:?}"
                ));
            }
            Ok(())
        }
        ActionSpec::KeyPress {
            key,
            hold_ms,
            repeat,
            interval_ms,
        } => {
            known_key(key)?;
            if hold_ms.is_some_and(|h| h > MAX_KEY_HOLD_MS) {
                return Err(format!("key_press: hold_ms больше {MAX_KEY_HOLD_MS}"));
            }
            if let Some(r) = repeat
                && !(1..=MAX_KEY_REPEAT).contains(r)
            {
                return Err(format!(
                    "key_press: repeat = {r} вне диапазона 1–{MAX_KEY_REPEAT}"
                ));
            }
            if interval_ms.is_some_and(|i| i > MAX_KEY_HOLD_MS) {
                return Err(format!("key_press: interval_ms больше {MAX_KEY_HOLD_MS}"));
            }
            Ok(())
        }
        ActionSpec::Hotkey { keys, hold_ms } => {
            if !(1..=5).contains(&keys.len()) {
                return Err("hotkey: от 1 до 5 клавиш".into());
            }
            for key in keys {
                known_key(key)?;
            }
            let unique: HashSet<String> = keys.iter().map(|k| k.to_ascii_lowercase()).collect();
            if unique.len() != keys.len() {
                return Err("hotkey: клавиши повторяются".into());
            }
            if hold_ms.is_some_and(|h| h > MAX_KEY_HOLD_MS) {
                return Err(format!("hotkey: hold_ms больше {MAX_KEY_HOLD_MS}"));
            }
            Ok(())
        }
        ActionSpec::SystemPower { .. } => Ok(()),
        ActionSpec::Wait { ms } => {
            if *ms > MAX_WAIT_MS {
                return Err(format!("wait: ms больше {MAX_WAIT_MS}"));
            }
            Ok(())
        }
        ActionSpec::Sequence { steps } => {
            if in_sequence {
                return Err("sequence: вложенные цепочки не поддерживаются".into());
            }
            if !(1..=MAX_SEQUENCE_STEPS).contains(&steps.len()) {
                return Err(format!("sequence: от 1 до {MAX_SEQUENCE_STEPS} шагов"));
            }
            for (i, step) in steps.iter().enumerate() {
                validate_action(step, dir, slots, true)
                    .map_err(|e| format!("шаг {}: {e}", i + 1))?;
                // После вопроса цепочка не продолжается: дальше решает ответ.
                if matches!(step, ActionSpec::Ask { .. }) && i + 1 != steps.len() {
                    return Err(format!("шаг {}: ask — только последним шагом", i + 1));
                }
            }
            Ok(())
        }
        ActionSpec::WaitProcess { name, timeout_sec } => {
            let trimmed = name.trim();
            let plain_name = !trimmed.is_empty()
                && !trimmed.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']);
            if !plain_name || !trimmed.to_ascii_lowercase().ends_with(".exe") {
                return Err(format!(
                    "wait_process: name = «{name}» — нужно имя файла программы, например faceitclient.exe"
                ));
            }
            if !(1..=MAX_PROCESS_WAIT_SEC).contains(timeout_sec) {
                return Err(format!(
                    "wait_process: timeout_sec = {timeout_sec} вне диапазона 1–{MAX_PROCESS_WAIT_SEC}"
                ));
            }
            Ok(())
        }
        ActionSpec::Ask { file, choices } => {
            let path = inside_dir(dir, file)?;
            if !path.is_file() {
                return Err(format!("ask: файла {} нет", path.display()));
            }
            if !(1..=MAX_ASK_CHOICES).contains(&choices.len()) {
                return Err(format!("ask: от 1 до {MAX_ASK_CHOICES} вариантов ответа"));
            }
            for (answer, id) in choices {
                if jarvis_nlu::normalize(answer).is_empty() {
                    return Err(format!("ask: ответ «{answer}» после нормализации пустой"));
                }
                if id.is_empty()
                    || !id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(format!("ask: «{answer}» ведёт к недопустимому id `{id}`"));
                }
            }
            Ok(())
        }
    }
}

/// Подходит ли заголовок вкладки к шаблону: `*` — любые символы, регистр
/// не важен, остальное — буквально и целиком («* - YouTube», «Google Календарь*»).
pub fn title_matches(pattern: &str, title: &str) -> bool {
    let pattern: Vec<char> = pattern.trim().to_lowercase().chars().collect();
    let title: Vec<char> = title.trim().to_lowercase().chars().collect();
    // Классический жадный разбор со звёздочкой и возвратом.
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < title.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == title[t] {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Спецсимволы cmd.exe, недопустимые в аргументах .bat/.cmd.
pub const BATCH_METACHARS: &[char] = &['&', '|', '<', '>', '^', '%', '"', '\n', '\r'];

/// Чем запускать скрипт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Exe,
    Batch,
    PowerShell,
    Python,
}

impl ScriptKind {
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        Some(match ext.as_str() {
            "exe" => ScriptKind::Exe,
            "bat" | "cmd" => ScriptKind::Batch,
            "ps1" => ScriptKind::PowerShell,
            "py" => ScriptKind::Python,
            _ => return None,
        })
    }
}

fn known_key(key: &str) -> Result<(), String> {
    crate::keys::vk_code(key)
        .map(|_| ())
        .ok_or_else(|| format!("неизвестная клавиша «{key}»"))
}

fn no_slots_in_path(path: &str) -> Result<(), String> {
    if path.contains('{') {
        return Err(format!(
            "в пути «{path}» нельзя подставлять слоты — только в аргументы"
        ));
    }
    Ok(())
}

/// Уровень громкости: число 0–100 или числовой слот в пределах 0–100.
fn validate_level(level: &str, slots: &BTreeMap<String, SlotSpec>) -> Result<(), String> {
    let names = placeholders(level);
    match names.as_slice() {
        [] => match level.trim().parse::<u32>() {
            Ok(0..=100) => Ok(()),
            _ => Err(format!("media: level = «{level}» — нужно число 0–100")),
        },
        [name] if level.trim() == format!("{{{name}}}") => match slots.get(*name) {
            Some(SlotSpec::Number { min, max, .. }) if *min >= 0 && *max <= 100 => Ok(()),
            Some(SlotSpec::Number { .. }) => {
                Err(format!("media: слот `{name}` должен быть в пределах 0–100"))
            }
            Some(_) => Err(format!("media: слот `{name}` должен быть числом")),
            None => Err(format!("media: слот `{name}` не объявлен")),
        },
        _ => Err(format!("media: level = «{level}» — число или один слот")),
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
                url: "https://www.youtube.com".into(),
                find_tab: Vec::new(),
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

    /// Манифест с одной командой и заданным действием (и слотами) в папке `dir`.
    fn one(dir: &Path, phrases: &str, slots: &str, action: &str) -> Registry {
        let text = format!(
            "pack = \"t\"\nversion = 1\n[[command]]\nid = \"c\"\nphrases = [{phrases}]\n\
             description = \"d\"\n{slots}\n[command.action]\n{action}\n"
        );
        let mut r = Registry::default();
        r.add_manifest(&dir.join("command.toml"), &text);
        r
    }

    fn ok(r: &Registry) {
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.commands.len(), 1);
    }

    fn fails(r: &Registry, needle: &str) {
        assert!(r.commands.is_empty(), "ожидали ошибку «{needle}»");
        assert!(r.errors[0].message.contains(needle), "{:?}", r.errors);
    }

    #[test]
    fn run_script_inside_pack_with_known_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ps1"), "").unwrap();
        std::fs::write(dir.path().join("b.bat"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();
        let d = dir.path();
        ok(&one(
            d,
            "\"скрипт\"",
            "",
            "type = \"run_script\"\npath = \"a.ps1\"\ntimeout_sec = 5",
        ));
        fails(
            &one(
                d,
                "\"скрипт\"",
                "",
                "type = \"run_script\"\npath = \"нет.ps1\"",
            ),
            "нет",
        );
        fails(
            &one(
                d,
                "\"скрипт\"",
                "",
                "type = \"run_script\"\npath = \"c.txt\"",
            ),
            "поддерживаются",
        );
        fails(
            &one(
                d,
                "\"скрипт\"",
                "",
                "type = \"run_script\"\npath = \"../a.ps1\"",
            ),
            "внутри",
        );
        fails(
            &one(
                d,
                "\"скрипт\"",
                "",
                "type = \"run_script\"\npath = \"a.ps1\"\ntimeout_sec = 0",
            ),
            "timeout_sec",
        );
        fails(
            &one(
                d,
                "\"скрипт\"",
                "",
                "type = \"run_script\"\npath = \"b.bat\"\nargs = [\"x & calc\"]",
            ),
            "недопустимы",
        );
    }

    #[test]
    fn launch_app_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.exe"), "").unwrap();
        let d = dir.path();
        ok(&one(
            d,
            "\"блокнот\"",
            "",
            "type = \"launch_app\"\npath = \"notepad.exe\"\nargs = [\"a.txt\"]",
        ));
        ok(&one(
            d,
            "\"тул\"",
            "",
            "type = \"launch_app\"\npath = \"./tool.exe\"",
        ));
        fails(
            &one(
                d,
                "\"тул\"",
                "",
                "type = \"launch_app\"\npath = \"bin/../tool.exe\"",
            ),
            "внутри",
        );
        fails(
            &one(
                d,
                "\"нет\"",
                "",
                "type = \"launch_app\"\npath = \"C:\\\\нет\\\\x.exe\"",
            ),
            "нет",
        );
        fails(
            &one(d, "\"пусто\"", "", "type = \"launch_app\"\npath = \"\""),
            "пустой",
        );
    }

    #[test]
    fn keys_and_hotkeys() {
        let d = Path::new(".");
        ok(&one(
            d,
            "\"прыжок\"",
            "",
            "type = \"key_press\"\nkey = \"space\"\nhold_ms = 100\nrepeat = 2",
        ));
        ok(&one(
            d,
            "\"сохрани\"",
            "",
            "type = \"hotkey\"\nkeys = [\"ctrl\", \"s\"]",
        ));
        fails(
            &one(d, "\"x\"", "", "type = \"key_press\"\nkey = \"банан\""),
            "неизвестная клавиша",
        );
        fails(
            &one(
                d,
                "\"x\"",
                "",
                "type = \"hotkey\"\nkeys = [\"ctrl\", \"ctrl\"]",
            ),
            "повторяются",
        );
        fails(
            &one(d, "\"x\"", "", "type = \"hotkey\"\nkeys = []"),
            "от 1 до 5",
        );
        fails(
            &one(
                d,
                "\"x\"",
                "",
                "type = \"key_press\"\nkey = \"a\"\nrepeat = 0",
            ),
            "repeat",
        );
    }

    #[test]
    fn sequence_and_wait() {
        let d = Path::new(".");
        let seq = "type = \"sequence\"\n[[command.action.steps]]\ntype = \"hotkey\"\nkeys = [\"win\", \"d\"]\n\
                   [[command.action.steps]]\ntype = \"wait\"\nms = 300\n\
                   [[command.action.steps]]\ntype = \"open_url\"\nurl = \"https://ya.ru\"";
        let r = one(d, "\"рабочий стол\"", "", seq);
        ok(&r);
        assert!(r.commands[0].action.summary().contains("затем"));
        let nested = "type = \"sequence\"\n[[command.action.steps]]\ntype = \"sequence\"\n\
                      [[command.action.steps.steps]]\ntype = \"wait\"\nms = 1";
        fails(&one(d, "\"x\"", "", nested), "вложенные");
        let bad_step =
            "type = \"sequence\"\n[[command.action.steps]]\ntype = \"wait\"\nms = 999999";
        fails(&one(d, "\"x\"", "", bad_step), "шаг 1");
    }

    #[test]
    fn slots_are_declared_used_and_kept_out_of_paths() {
        let d = Path::new(".");
        let level = "[command.slots.level]\ntype = \"number\"\nmin = 0\nmax = 100";
        let set = "type = \"media\"\noperation = \"volume_set\"\nlevel = \"{level}\"";
        let r = one(d, "\"громкость на {level}\"", level, set);
        ok(&r);
        assert!(r.commands[0].slots.contains_key("level"));

        fails(&one(d, "\"громкость\"", level, set), "не встречается");
        fails(
            &one(d, "\"громкость на {level} {lvl}\"", level, set),
            "не объявлен",
        );
        let wide = "[command.slots.level]\ntype = \"number\"\nmin = 0\nmax = 1000";
        fails(&one(d, "\"громкость на {level}\"", wide, set), "0–100");
        fails(
            &one(
                d,
                "\"громкость на {level}\"",
                level,
                "type = \"media\"\noperation = \"volume_set\"",
            ),
            "требует level",
        );
        fails(
            &one(
                d,
                "\"запусти {level}\"",
                level,
                "type = \"launch_app\"\npath = \"{level}.exe\"",
            ),
            "нельзя подставлять",
        );
    }

    #[test]
    fn slot_values_fill_action() {
        let d = Path::new(".");
        let site =
            "[command.slots.site]\ntype = \"choice\"\nvalues = { \"ютуб\" = \"youtube.com\" }";
        let r = one(
            d,
            "\"открой сайт {site}\"",
            site,
            "type = \"open_url\"\nurl = \"https://{site}\"",
        );
        ok(&r);
        let values: crate::slots::SlotValues = [("site".to_owned(), "youtube.com".to_owned())]
            .into_iter()
            .collect();
        assert_eq!(
            r.commands[0].action.with_slots(&values),
            ActionSpec::OpenUrl {
                url: "https://youtube.com".into(),
                find_tab: Vec::new(),
            }
        );
    }

    #[test]
    fn power_operations() {
        let d = Path::new(".");
        for op in ["lock", "sleep", "logoff", "restart", "shutdown"] {
            ok(&one(
                d,
                "\"питание\"",
                "",
                &format!("type = \"system_power\"\noperation = \"{op}\""),
            ));
        }
        fails(
            &one(
                d,
                "\"x\"",
                "",
                "type = \"system_power\"\noperation = \"explode\"",
            ),
            "explode",
        );
    }

    #[test]
    fn tab_title_patterns() {
        assert!(title_matches("* - YouTube", "(96) МЕНТАЛИСТ - YouTube"));
        assert!(title_matches("youtube", "YouTube"));
        assert!(!title_matches("* - YouTube", "YouTube Music"));
        assert!(!title_matches("YouTube", "YouTube Music"));
        assert!(title_matches(
            "Google Календарь*",
            "Google Календарь – неделя"
        ));
        assert!(title_matches("*a*b*", "xaYYbZ"));
        assert!(!title_matches("*a*b", "xaYYbZ"));
        assert!(title_matches("*", ""));
    }

    fn manifest_with(action: &str) -> Registry {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("q.wav"), b"RIFF").unwrap();
        let text = format!(
            "pack = \"t\"\nversion = 1\n[[command]]\nid = \"a\"\nphrases = [\"запусти игровой режим\"]\n\
             description = \"x\"\n{action}\n\
             [[command]]\nid = \"game_cs2\"\nphrases = [\"запусти кс\"]\ndescription = \"y\"\n\
             [command.action]\ntype = \"wait\"\nms = 1\n"
        );
        let mut r = Registry::default();
        r.add_manifest(&dir.path().join(MANIFEST_FILE), &text);
        r.check_ask_targets();
        r
    }

    #[test]
    fn ask_and_wait_process_are_validated() {
        let ok = manifest_with(
            "[command.action]\ntype = \"sequence\"\n\
             [[command.action.steps]]\ntype = \"wait_process\"\nname = \"steam.exe\"\ntimeout_sec = 60\n\
             [[command.action.steps]]\ntype = \"ask\"\nfile = \"q.wav\"\nchoices = { \"кс\" = \"game_cs2\" }\n",
        );
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
        assert_eq!(ok.get("a").unwrap().action.ask_targets(), ["game_cs2"]);

        let not_last = manifest_with(
            "[command.action]\ntype = \"sequence\"\n\
             [[command.action.steps]]\ntype = \"ask\"\nfile = \"q.wav\"\nchoices = { \"кс\" = \"game_cs2\" }\n\
             [[command.action.steps]]\ntype = \"wait\"\nms = 1\n",
        );
        assert!(
            not_last.errors[0].message.contains("последним"),
            "{:?}",
            not_last.errors
        );

        let unknown = manifest_with(
            "[command.action]\ntype = \"ask\"\nfile = \"q.wav\"\nchoices = { \"кс\" = \"nope\" }\n",
        );
        assert!(unknown.get("a").is_none());
        assert!(
            unknown.errors[0].message.contains("nope"),
            "{:?}",
            unknown.errors
        );

        for bad in [
            "name = \"C:\\\\x\\\\steam.exe\"\ntimeout_sec = 5",
            "name = \"steam\"\ntimeout_sec = 5",
            "name = \"steam.exe\"\ntimeout_sec = 0",
            "name = \"steam.exe\"\ntimeout_sec = 9999",
        ] {
            let r = manifest_with(&format!(
                "[command.action]\ntype = \"wait_process\"\n{bad}\n"
            ));
            assert!(!r.errors.is_empty(), "{bad}");
        }
    }

    #[test]
    fn find_tab_is_optional_and_checked() {
        let ok = manifest_with(
            "[command.action]\ntype = \"open_url\"\nurl = \"https://youtube.com\"\nfind_tab = [\"* - YouTube\"]\n",
        );
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
        let any = manifest_with(
            "[command.action]\ntype = \"open_url\"\nurl = \"https://youtube.com\"\nfind_tab = [\" * \"]\n",
        );
        assert!(!any.errors.is_empty());
        // Без find_tab отпечаток прежний — сохранённые разрешения не слетают.
        let plain = ActionSpec::OpenUrl {
            url: "https://youtube.com".into(),
            find_tab: Vec::new(),
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"type":"open_url","url":"https://youtube.com"}"#
        );
    }
}
