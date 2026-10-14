//! Исполнители действий команд (раздел 6 ТЗ).
//!
//! Системные вызовы спрятаны за трейтом [`System`]: в работе — реализация
//! из `jarvis-platform`, в тестах — подделка, записывающая вызовы.
//! Параметры уже проверены при загрузке манифеста (`jarvis_core::commands`),
//! слоты подставлены; здесь — только исполнение.
//!
//! Каждое действие можно прервать ([`CancelToken`]): аварийная остановка
//! прерывает ожидания и цепочки, завершает скрипты.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use jarvis_core::commands::{
    ActionSpec, DEFAULT_SCRIPT_TIMEOUT_SEC, MediaOp, PowerOp, ScriptKind, title_matches,
    validate_url,
};

/// Громкость по умолчанию меняется на 10 % (одно нажатие ≈ 2 %).
pub const DEFAULT_VOLUME_STEPS: u32 = 5;
pub const DEFAULT_KEY_HOLD_MS: u64 = 30;
pub const DEFAULT_KEY_INTERVAL_MS: u64 = 50;
/// Как часто проверять, запустился ли процесс (`wait_process`).
pub const PROCESS_POLL: Duration = Duration::from_millis(500);
/// Через сколько после открытия адреса выводить браузер вперёд: браузер
/// успевает принять адрес и открыть вкладку.
pub const BROWSER_FOCUS_DELAY: Duration = Duration::from_millis(300);

/// Флаг отмены действия; клонируется и передаётся между потоками.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Сам флаг — для платформенного кода, который ждёт процесс.
    pub fn flag(&self) -> &AtomicBool {
        &self.0
    }

    /// Ждёт `duration` или отмену. `false` — отменили.
    pub fn sleep(&self, duration: Duration) -> bool {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            if self.is_cancelled() {
                return false;
            }
            std::thread::sleep((until - Instant::now()).min(Duration::from_millis(20)));
        }
        !self.is_cancelled()
    }
}

/// Процесс для запуска: программа и список аргументов — без строки оболочки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    /// Не показывать окно консоли (для скриптов).
    pub hide_window: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEnd {
    Exited(i32),
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessResult {
    pub end: ProcessEnd,
    /// Хвост вывода (stdout + stderr) — для журнала при ошибке.
    pub output: String,
}

/// Системные возможности, нужные исполнителям.
pub trait System: Send + Sync {
    fn open_url(&self, url: &str) -> Result<(), String>;
    fn media_key(&self, op: MediaOp, presses: u32) -> Result<(), String>;
    /// Громкость системы, 0–100.
    fn set_volume(&self, percent: u32) -> Result<(), String>;
    /// Запустить и не ждать.
    fn spawn_detached(&self, spec: &ProcessSpec) -> Result<(), String>;
    /// Запустить и дождаться: с тайм-аутом и отменой (дерево процессов завершается).
    fn run_process(
        &self,
        spec: &ProcessSpec,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<ProcessResult, String>;
    /// Нажать клавиши по порядку, подержать, отпустить в обратном порядке.
    /// Отпускание гарантировано (и аварийной остановкой тоже).
    fn press_keys(&self, vks: &[u16], hold: Duration) -> Result<(), String>;
    fn power(&self, op: PowerOp) -> Result<(), String>;
    /// Найти открытую вкладку браузера, заголовок которой подходит, сделать её
    /// текущей и вывести окно вперёд. `Ok(None)` — такой вкладки нет.
    fn activate_tab(&self, matches: &dyn Fn(&str) -> bool) -> Result<Option<String>, String>;
    /// Вывести вперёд главное окно запущенной программы (по имени процесса);
    /// `false` — не запущена или окна не видно.
    fn focus_app(&self, name: &str) -> Result<bool, String>;
    /// Вывести окно браузера вперёд (после открытия новой вкладки);
    /// `false` — браузер не найден или Windows не дала сменить окно.
    fn focus_browser(&self) -> bool;
    /// Запущен ли процесс с таким именем файла (`steam.exe`), без учёта регистра.
    fn process_running(&self, name: &str) -> Result<bool, String>;
    /// Закрыть окна программы (по имени процесса), как крестиком; сколько
    /// окон попросили закрыться (0 — не запущена или окон не видно).
    fn close_app(&self, name: &str) -> Result<usize, String>;
}

/// Чем закончилось действие.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// `say`: что проиграть (проигрывает ядро — у него звук).
    Play(Sound),
    /// `ask`: задать вопрос и ждать ответа — дальше решает ядро.
    Ask(Question),
    /// `dictate_note`: выслушать текст и дописать в заметку — дальше ядро.
    Dictate(NoteTarget),
}

/// Заметка Google Keep, куда дописывать надиктованное.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteTarget {
    pub url: String,
    /// Заголовок — по нему находится текст заметки на странице.
    pub title: String,
    /// Начало строки («- »).
    pub prefix: String,
}

/// Вопрос с вариантами ответа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// WAV вопроса (путь уже проверен и абсолютный).
    pub sound: PathBuf,
    /// Как отвечают → id команды.
    pub choices: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sound {
    /// Ответ из звукового пакета по ключу.
    Reply(String),
    /// WAV из папки пакета команд (путь уже проверен и абсолютный).
    File(PathBuf),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionError {
    #[error("не удалось открыть адрес: {0}")]
    OpenUrl(String),
    #[error("не удалось нажать мультимедиа-клавишу: {0}")]
    Media(String),
    #[error("не удалось изменить громкость: {0}")]
    Volume(String),
    #[error("не удалось запустить: {0}")]
    Spawn(String),
    #[error("скрипт завершился с кодом {code}{}", tail(output))]
    ExitCode { code: i32, output: String },
    #[error("скрипт не уложился в {0} с и остановлен")]
    Timeout(u64),
    #[error("прервано аварийной остановкой")]
    Cancelled,
    #[error("не удалось нажать клавиши: {0}")]
    Keys(String),
    #[error("не удалось выполнить действие с питанием: {0}")]
    Power(String),
    #[error("недопустимые параметры действия: {0}")]
    Invalid(String),
    #[error("{name} не запустился за {secs} с")]
    ProcessNotStarted { name: String, secs: u64 },
    #[error("не удалось проверить процессы: {0}")]
    Processes(String),
    #[error("не удалось закрыть окна: {0}")]
    Close(String),
}

fn tail(output: &str) -> String {
    let output = output.trim();
    if output.is_empty() {
        String::new()
    } else {
        format!(": {output}")
    }
}

/// Обстановка выполнения.
pub struct Context<'a> {
    /// Папка пакета команды.
    pub dir: &'a Path,
    pub system: &'a dyn System,
    pub cancel: &'a CancelToken,
    /// Проиграть звук посреди цепочки (`say` внутри `sequence`).
    pub play: &'a (dyn Fn(Sound) + Sync),
}

/// Выполняет действие (слоты уже подставлены).
pub fn execute(action: &ActionSpec, ctx: &Context<'_>) -> Result<Outcome, ActionError> {
    if ctx.cancel.is_cancelled() {
        return Err(ActionError::Cancelled);
    }
    match action {
        ActionSpec::Say {
            reply: Some(key), ..
        } => Ok(Outcome::Play(Sound::Reply(key.clone()))),
        ActionSpec::Say {
            file: Some(file), ..
        } => Ok(Outcome::Play(Sound::File(ctx.dir.join(file)))),
        ActionSpec::Say { .. } => Err(ActionError::Invalid("say без reply и file".into())),
        ActionSpec::OpenUrl { url, find_tab } => {
            // Повторная проверка — после подстановки слотов адрес мог измениться.
            validate_url(url).map_err(ActionError::Invalid)?;
            if !find_tab.is_empty() {
                let matches = |title: &str| find_tab.iter().any(|p| title_matches(p, title));
                match ctx.system.activate_tab(&matches) {
                    Ok(Some(title)) => {
                        tracing::info!(%title, "вкладка уже открыта — переключаюсь");
                        return Ok(Outcome::Done);
                    }
                    Ok(None) => {}
                    // Не нашли из-за сбоя — не беда: просто откроем адрес.
                    Err(e) => tracing::warn!(error = %e, "не удалось поискать вкладку"),
                }
            }
            ctx.system.open_url(url).map_err(ActionError::OpenUrl)?;
            // Новая вкладка открывается в окне браузера, но Windows не выводит
            // его вперёд сама — делаем это (не вышло — не ошибка команды).
            if ctx.cancel.sleep(BROWSER_FOCUS_DELAY) && !ctx.system.focus_browser() {
                tracing::debug!("браузер не вышел вперёд");
            }
            Ok(Outcome::Done)
        }
        ActionSpec::Media {
            operation: MediaOp::VolumeSet,
            level,
            ..
        } => {
            let percent = level
                .as_deref()
                .and_then(|l| l.trim().parse::<u32>().ok())
                .filter(|p| *p <= 100)
                .ok_or_else(|| ActionError::Invalid(format!("громкость «{level:?}»")))?;
            ctx.system
                .set_volume(percent)
                .map_err(ActionError::Volume)?;
            Ok(Outcome::Done)
        }
        ActionSpec::Media {
            operation, steps, ..
        } => {
            let presses = if operation.is_volume() {
                steps.unwrap_or(DEFAULT_VOLUME_STEPS)
            } else {
                1
            };
            ctx.system
                .media_key(*operation, presses)
                .map_err(ActionError::Media)?;
            Ok(Outcome::Done)
        }
        ActionSpec::LaunchApp {
            path,
            args,
            working_dir,
            focus,
        } => {
            if let Some(name) = focus {
                match ctx.system.focus_app(name) {
                    Ok(true) => {
                        tracing::info!(%name, "программа уже открыта — вывожу окно вперёд");
                        return Ok(Outcome::Done);
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!(error = %e, "не удалось поискать окно программы"),
                }
            }
            let spec = ProcessSpec {
                program: app_path(ctx.dir, path),
                args: args.clone(),
                working_dir: working_dir.clone(),
                hide_window: false,
            };
            ctx.system
                .spawn_detached(&spec)
                .map_err(ActionError::Spawn)?;
            Ok(Outcome::Done)
        }
        ActionSpec::CloseApp { process } => {
            let closed = ctx.system.close_app(process).map_err(ActionError::Close)?;
            if closed == 0 {
                // Не запущена или свёрнута в трей — закрывать нечего, это не ошибка.
                tracing::info!(%process, "окон программы не видно — закрывать нечего");
            } else {
                tracing::info!(%process, windows = closed, "попросил окна программы закрыться");
            }
            Ok(Outcome::Done)
        }
        ActionSpec::RunScript {
            path,
            args,
            timeout_sec,
        } => {
            let script = ctx.dir.join(path);
            let spec = script_process(&script, args, ctx.dir)?;
            let timeout = timeout_sec.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SEC);
            let result = ctx
                .system
                .run_process(&spec, Duration::from_secs(timeout), ctx.cancel)
                .map_err(ActionError::Spawn)?;
            match result.end {
                ProcessEnd::Exited(0) => Ok(Outcome::Done),
                ProcessEnd::Exited(code) => Err(ActionError::ExitCode {
                    code,
                    output: result.output,
                }),
                ProcessEnd::TimedOut => Err(ActionError::Timeout(timeout)),
                ProcessEnd::Cancelled => Err(ActionError::Cancelled),
            }
        }
        ActionSpec::KeyPress {
            key,
            hold_ms,
            repeat,
            interval_ms,
        } => {
            let vk = vk(key)?;
            let hold = Duration::from_millis(hold_ms.unwrap_or(DEFAULT_KEY_HOLD_MS));
            let interval = Duration::from_millis(interval_ms.unwrap_or(DEFAULT_KEY_INTERVAL_MS));
            let times = repeat.unwrap_or(1);
            for i in 0..times {
                if i > 0 && !ctx.cancel.sleep(interval) {
                    return Err(ActionError::Cancelled);
                }
                ctx.system
                    .press_keys(&[vk], hold)
                    .map_err(ActionError::Keys)?;
            }
            Ok(Outcome::Done)
        }
        ActionSpec::Hotkey { keys, hold_ms } => {
            let vks = keys.iter().map(|k| vk(k)).collect::<Result<Vec<_>, _>>()?;
            let hold = Duration::from_millis(hold_ms.unwrap_or(DEFAULT_KEY_HOLD_MS));
            ctx.system
                .press_keys(&vks, hold)
                .map_err(ActionError::Keys)?;
            Ok(Outcome::Done)
        }
        ActionSpec::SystemPower { operation } => {
            ctx.system.power(*operation).map_err(ActionError::Power)?;
            Ok(Outcome::Done)
        }
        ActionSpec::Wait { ms } => {
            if ctx.cancel.sleep(Duration::from_millis(*ms)) {
                Ok(Outcome::Done)
            } else {
                Err(ActionError::Cancelled)
            }
        }
        ActionSpec::Sequence { steps } => {
            for step in steps {
                match execute(step, ctx)? {
                    Outcome::Play(sound) => (ctx.play)(sound),
                    // Вопрос — всегда последний шаг (проверено в манифесте).
                    Outcome::Ask(question) => return Ok(Outcome::Ask(question)),
                    // В цепочке диктовки не бывает (проверено в манифесте).
                    Outcome::Dictate(target) => return Ok(Outcome::Dictate(target)),
                    Outcome::Done => {}
                }
            }
            Ok(Outcome::Done)
        }
        ActionSpec::WaitProcess { name, timeout_sec } => {
            let deadline = Instant::now() + Duration::from_secs(*timeout_sec);
            loop {
                if ctx
                    .system
                    .process_running(name)
                    .map_err(ActionError::Processes)?
                {
                    return Ok(Outcome::Done);
                }
                if Instant::now() >= deadline {
                    return Err(ActionError::ProcessNotStarted {
                        name: name.clone(),
                        secs: *timeout_sec,
                    });
                }
                if !ctx.cancel.sleep(PROCESS_POLL) {
                    return Err(ActionError::Cancelled);
                }
            }
        }
        ActionSpec::DictateNote { url, title, prefix } => Ok(Outcome::Dictate(NoteTarget {
            url: url.trim().to_owned(),
            title: title.trim().to_owned(),
            prefix: prefix.clone(),
        })),
        ActionSpec::Ask { file, choices } => Ok(Outcome::Ask(Question {
            sound: ctx.dir.join(file),
            choices: choices
                .iter()
                .map(|(answer, id)| (answer.clone(), id.clone()))
                .collect(),
        })),
    }
}

fn vk(key: &str) -> Result<u16, ActionError> {
    jarvis_core::keys::vk_code(key).ok_or_else(|| ActionError::Invalid(format!("клавиша «{key}»")))
}

/// Путь программы: полный — как есть; с папками — внутри пакета; голое
/// имя — ищется в PATH при запуске.
fn app_path(dir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path.trim());
    if p.is_absolute() || !path.contains(['\\', '/']) {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

/// Чем запустить скрипт. Аргументы — списком, без строки оболочки
/// (`.bat` Rust запускает через cmd.exe с безопасным экранированием).
fn script_process(script: &Path, args: &[String], dir: &Path) -> Result<ProcessSpec, ActionError> {
    let kind = ScriptKind::of(script)
        .ok_or_else(|| ActionError::Invalid(format!("тип скрипта {}", script.display())))?;
    let script_arg = script.to_string_lossy().into_owned();
    let (program, mut full_args): (PathBuf, Vec<String>) = match kind {
        ScriptKind::Exe | ScriptKind::Batch => (script.to_path_buf(), Vec::new()),
        ScriptKind::PowerShell => (
            "powershell.exe".into(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                script_arg,
            ],
        ),
        ScriptKind::Python => ("python.exe".into(), vec![script_arg]),
    };
    full_args.extend(args.iter().cloned());
    Ok(ProcessSpec {
        program,
        args: full_args,
        working_dir: Some(dir.to_path_buf()),
        hide_window: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        fail: bool,
        process_end: Option<ProcessEnd>,
        /// Заголовки открытых вкладок.
        tabs: Vec<String>,
        /// Через сколько проверок процесс «запустится» (`None` — никогда).
        process_after: Option<usize>,
        /// Программы с открытым окном (для `focus`).
        running: Vec<String>,
        checks: Mutex<usize>,
    }

    impl Fake {
        fn log(&self, s: String) -> Result<(), String> {
            self.calls.lock().unwrap().push(s);
            if self.fail {
                Err("сбой".into())
            } else {
                Ok(())
            }
        }
    }

    impl System for Fake {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.log(format!("open {url}"))
        }
        fn media_key(&self, op: MediaOp, presses: u32) -> Result<(), String> {
            self.log(format!("{op:?}×{presses}"))
        }
        fn set_volume(&self, percent: u32) -> Result<(), String> {
            self.log(format!("volume {percent}"))
        }
        fn spawn_detached(&self, spec: &ProcessSpec) -> Result<(), String> {
            self.log(format!("spawn {} {:?}", spec.program.display(), spec.args))
        }
        fn run_process(
            &self,
            spec: &ProcessSpec,
            timeout: Duration,
            _: &CancelToken,
        ) -> Result<ProcessResult, String> {
            self.log(format!(
                "run {} {:?} {}s",
                spec.program.display(),
                spec.args,
                timeout.as_secs()
            ))?;
            Ok(ProcessResult {
                end: self.process_end.clone().unwrap_or(ProcessEnd::Exited(0)),
                output: "вывод".into(),
            })
        }
        fn press_keys(&self, vks: &[u16], hold: Duration) -> Result<(), String> {
            self.log(format!("keys {vks:?} {}ms", hold.as_millis()))
        }
        fn power(&self, op: PowerOp) -> Result<(), String> {
            self.log(format!("power {op:?}"))
        }
        fn activate_tab(&self, matches: &dyn Fn(&str) -> bool) -> Result<Option<String>, String> {
            let found = self.tabs.iter().find(|t| matches(t)).cloned();
            self.log(format!("tab {found:?}"))?;
            Ok(found)
        }
        fn focus_browser(&self) -> bool {
            let _ = self.log("focus browser".into());
            true
        }
        fn focus_app(&self, name: &str) -> Result<bool, String> {
            self.log(format!("focus {name}"))?;
            Ok(self.running.iter().any(|r| r.eq_ignore_ascii_case(name)))
        }
        fn process_running(&self, name: &str) -> Result<bool, String> {
            let mut checks = self.checks.lock().unwrap();
            *checks += 1;
            self.log(format!("check {name}"))?;
            Ok(self.process_after.is_some_and(|n| *checks > n))
        }

        fn close_app(&self, name: &str) -> Result<usize, String> {
            self.log(format!("close {name}"))?;
            Ok(usize::from(self.running.iter().any(|r| r == name)))
        }
    }

    fn calls(f: &Fake) -> Vec<String> {
        f.calls.lock().unwrap().clone()
    }

    fn run(action: &ActionSpec, fake: &Fake) -> Result<Outcome, ActionError> {
        run_with(action, fake, &CancelToken::default())
    }

    fn run_with(
        action: &ActionSpec,
        fake: &Fake,
        cancel: &CancelToken,
    ) -> Result<Outcome, ActionError> {
        let played = Mutex::new(Vec::new());
        let play = |s: Sound| played.lock().unwrap().push(s);
        let ctx = Context {
            dir: Path::new("C:\\pack"),
            system: fake,
            cancel,
            play: &play,
        };
        execute(action, &ctx)
    }

    #[test]
    fn open_url_calls_system_and_rechecks_url() {
        let fake = Fake::default();
        let ok = ActionSpec::OpenUrl {
            url: "https://www.youtube.com".into(),
            find_tab: Vec::new(),
        };
        assert_eq!(run(&ok, &fake), Ok(Outcome::Done));
        let bad = ActionSpec::OpenUrl {
            url: "file:///C:/Windows/System32/cmd.exe".into(),
            find_tab: Vec::new(),
        };
        assert!(matches!(run(&bad, &fake), Err(ActionError::Invalid(_))));
        assert_eq!(
            calls(&fake),
            ["open https://www.youtube.com", "focus browser"]
        );
    }

    #[test]
    fn media_presses_once_volume_uses_steps_and_set() {
        let fake = Fake::default();
        for a in [
            ActionSpec::Media {
                operation: MediaOp::Next,
                steps: None,
                level: None,
            },
            ActionSpec::Media {
                operation: MediaOp::VolumeUp,
                steps: Some(3),
                level: None,
            },
            ActionSpec::Media {
                operation: MediaOp::VolumeDown,
                steps: None,
                level: None,
            },
            ActionSpec::Media {
                operation: MediaOp::VolumeSet,
                steps: None,
                level: Some("40".into()),
            },
        ] {
            run(&a, &fake).unwrap();
        }
        assert_eq!(
            calls(&fake),
            ["Next×1", "VolumeUp×3", "VolumeDown×5", "volume 40"]
        );
        let bad = ActionSpec::Media {
            operation: MediaOp::VolumeSet,
            steps: None,
            level: Some("{level}".into()),
        };
        assert!(
            matches!(run(&bad, &fake), Err(ActionError::Invalid(_))),
            "слот не подставлен"
        );
    }

    #[test]
    fn launch_app_resolves_paths() {
        let fake = Fake::default();
        let launch = |path: &str| ActionSpec::LaunchApp {
            path: path.into(),
            args: vec!["a.txt".into()],
            working_dir: None,
            focus: None,
        };
        run(&launch("notepad.exe"), &fake).unwrap();
        run(&launch("bin\\tool.exe"), &fake).unwrap();
        run(&launch("C:\\Apps\\x.exe"), &fake).unwrap();
        assert_eq!(
            calls(&fake),
            [
                "spawn notepad.exe [\"a.txt\"]",
                "spawn C:\\pack\\bin\\tool.exe [\"a.txt\"]",
                "spawn C:\\Apps\\x.exe [\"a.txt\"]"
            ]
        );
    }

    #[test]
    fn scripts_run_through_the_right_interpreter() {
        let fake = Fake::default();
        let script = |path: &str| ActionSpec::RunScript {
            path: path.into(),
            args: vec!["--x".into()],
            timeout_sec: Some(5),
        };
        run(&script("a.ps1"), &fake).unwrap();
        run(&script("b.bat"), &fake).unwrap();
        run(&script("c.exe"), &fake).unwrap();
        run(&script("d.py"), &fake).unwrap();
        let c = calls(&fake);
        assert!(
            c[0].starts_with("run powershell.exe [\"-NoProfile\""),
            "{}",
            c[0]
        );
        assert!(
            c[0].contains("C:\\\\pack\\\\a.ps1") && c[0].ends_with("\"--x\"] 5s"),
            "{}",
            c[0]
        );
        assert_eq!(c[1], "run C:\\pack\\b.bat [\"--x\"] 5s");
        assert_eq!(c[2], "run C:\\pack\\c.exe [\"--x\"] 5s");
        assert!(c[3].starts_with("run python.exe"), "{}", c[3]);
    }

    #[test]
    fn script_failures_are_reported() {
        let script = ActionSpec::RunScript {
            path: "a.ps1".into(),
            args: vec![],
            timeout_sec: None,
        };
        let failed = Fake {
            process_end: Some(ProcessEnd::Exited(2)),
            ..Fake::default()
        };
        assert_eq!(
            run(&script, &failed),
            Err(ActionError::ExitCode {
                code: 2,
                output: "вывод".into()
            })
        );
        let timed_out = Fake {
            process_end: Some(ProcessEnd::TimedOut),
            ..Fake::default()
        };
        assert_eq!(
            run(&script, &timed_out),
            Err(ActionError::Timeout(DEFAULT_SCRIPT_TIMEOUT_SEC))
        );
        let cancelled = Fake {
            process_end: Some(ProcessEnd::Cancelled),
            ..Fake::default()
        };
        assert_eq!(run(&script, &cancelled), Err(ActionError::Cancelled));
    }

    #[test]
    fn keys_and_hotkeys() {
        let fake = Fake::default();
        run(
            &ActionSpec::KeyPress {
                key: "space".into(),
                hold_ms: Some(100),
                repeat: Some(2),
                interval_ms: Some(1),
            },
            &fake,
        )
        .unwrap();
        run(
            &ActionSpec::Hotkey {
                keys: vec!["ctrl".into(), "s".into()],
                hold_ms: None,
            },
            &fake,
        )
        .unwrap();
        assert_eq!(
            calls(&fake),
            ["keys [32] 100ms", "keys [32] 100ms", "keys [162, 83] 30ms"]
        );
    }

    #[test]
    fn power_goes_to_system() {
        let fake = Fake::default();
        run(
            &ActionSpec::SystemPower {
                operation: PowerOp::Lock,
            },
            &fake,
        )
        .unwrap();
        assert_eq!(calls(&fake), ["power Lock"]);
    }

    #[test]
    fn sequence_runs_in_order_and_stops_on_error() {
        let fake = Fake::default();
        let seq = ActionSpec::Sequence {
            steps: vec![
                ActionSpec::Hotkey {
                    keys: vec!["win".into(), "d".into()],
                    hold_ms: None,
                },
                ActionSpec::Wait { ms: 1 },
                ActionSpec::OpenUrl {
                    url: "https://ya.ru".into(),
                    find_tab: Vec::new(),
                },
            ],
        };
        assert_eq!(run(&seq, &fake), Ok(Outcome::Done));
        assert_eq!(
            calls(&fake),
            ["keys [91, 68] 30ms", "open https://ya.ru", "focus browser"]
        );

        let failing = Fake {
            fail: true,
            ..Fake::default()
        };
        assert!(run(&seq, &failing).is_err());
        assert_eq!(calls(&failing).len(), 1, "после ошибки шаги не выполняются");
    }

    #[test]
    fn cancel_stops_wait_and_sequence() {
        let fake = Fake::default();
        let cancel = CancelToken::default();
        let c2 = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c2.cancel();
        });
        let started = Instant::now();
        let seq = ActionSpec::Sequence {
            steps: vec![
                ActionSpec::Wait { ms: 5_000 },
                ActionSpec::OpenUrl {
                    url: "https://ya.ru".into(),
                    find_tab: Vec::new(),
                },
            ],
        };
        assert_eq!(run_with(&seq, &fake, &cancel), Err(ActionError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(calls(&fake).is_empty());
        t.join().unwrap();
    }

    #[test]
    fn say_returns_sound_or_plays_inside_sequence() {
        let fake = Fake::default();
        assert_eq!(
            run(
                &ActionSpec::Say {
                    reply: Some("ok".into()),
                    file: None
                },
                &fake
            ),
            Ok(Outcome::Play(Sound::Reply("ok".into())))
        );
        let played = Mutex::new(Vec::new());
        let play = |s: Sound| played.lock().unwrap().push(s);
        let cancel = CancelToken::default();
        let ctx = Context {
            dir: Path::new("p"),
            system: &fake,
            cancel: &cancel,
            play: &play,
        };
        let seq = ActionSpec::Sequence {
            steps: vec![ActionSpec::Say {
                reply: None,
                file: Some("hi.wav".into()),
            }],
        };
        assert_eq!(execute(&seq, &ctx), Ok(Outcome::Done));
        assert_eq!(
            *played.lock().unwrap(),
            [Sound::File(Path::new("p").join("hi.wav"))]
        );
    }

    fn youtube(find_tab: &[&str]) -> ActionSpec {
        ActionSpec::OpenUrl {
            url: "https://www.youtube.com".into(),
            find_tab: find_tab.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn open_url_switches_to_existing_tab() {
        let fake = Fake {
            tabs: vec!["YouTube Music".into(), "(3) Видео - YouTube".into()],
            ..Fake::default()
        };
        assert_eq!(
            run(&youtube(&["* - YouTube", "YouTube"]), &fake),
            Ok(Outcome::Done)
        );
        assert_eq!(calls(&fake), ["tab Some(\"(3) Видео - YouTube\")"]);
    }

    #[test]
    fn open_url_opens_when_no_tab_matches() {
        let fake = Fake {
            tabs: vec!["YouTube Music".into()],
            ..Fake::default()
        };
        assert_eq!(
            run(&youtube(&["* - YouTube", "YouTube"]), &fake),
            Ok(Outcome::Done)
        );
        assert_eq!(
            calls(&fake),
            ["tab None", "open https://www.youtube.com", "focus browser"]
        );
        // Без find_tab вкладки не ищутся.
        let plain = Fake::default();
        run(&youtube(&[]), &plain).unwrap();
        assert_eq!(
            calls(&plain),
            ["open https://www.youtube.com", "focus browser"]
        );
    }

    #[test]
    fn wait_process_polls_until_started_or_timeout() {
        let started = Fake {
            process_after: Some(1),
            ..Fake::default()
        };
        let wait = |secs| ActionSpec::WaitProcess {
            name: "steam.exe".into(),
            timeout_sec: secs,
        };
        assert_eq!(run(&wait(5), &started), Ok(Outcome::Done));
        assert_eq!(calls(&started), ["check steam.exe", "check steam.exe"]);

        let never = Fake::default();
        assert_eq!(
            run(&wait(1), &never),
            Err(ActionError::ProcessNotStarted {
                name: "steam.exe".into(),
                secs: 1
            })
        );

        let cancel = CancelToken::default();
        let canceller = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        assert_eq!(
            run_with(&wait(60), &Fake::default(), &cancel),
            Err(ActionError::Cancelled)
        );
    }

    #[test]
    fn ask_ends_sequence_with_question() {
        let fake = Fake::default();
        let action = ActionSpec::Sequence {
            steps: vec![
                ActionSpec::LaunchApp {
                    path: "steam.exe".into(),
                    args: vec![],
                    working_dir: None,
                    focus: None,
                },
                ActionSpec::Ask {
                    file: "ask.wav".into(),
                    choices: [("кс".to_owned(), "game_cs2".to_owned())]
                        .into_iter()
                        .collect(),
                },
            ],
        };
        assert_eq!(
            run(&action, &fake),
            Ok(Outcome::Ask(Question {
                sound: PathBuf::from("C:\\pack\\ask.wav"),
                choices: vec![("кс".into(), "game_cs2".into())],
            }))
        );
        assert_eq!(calls(&fake), ["spawn steam.exe []"]);
    }

    #[test]
    fn launch_with_focus_brings_running_app_forward() {
        let firefox = ActionSpec::LaunchApp {
            path: "C:\\Firefox\\firefox.exe".into(),
            args: vec![],
            working_dir: None,
            focus: Some("firefox.exe".into()),
        };
        let open = Fake {
            running: vec!["Firefox.exe".into()],
            ..Fake::default()
        };
        assert_eq!(run(&firefox, &open), Ok(Outcome::Done));
        assert_eq!(calls(&open), ["focus firefox.exe"]);

        let closed = Fake::default();
        assert_eq!(run(&firefox, &closed), Ok(Outcome::Done));
        assert_eq!(
            calls(&closed),
            ["focus firefox.exe", "spawn C:\\Firefox\\firefox.exe []"]
        );
    }

    #[test]
    fn close_app_closes_windows_and_is_quiet_when_nothing_to_close() {
        let close = ActionSpec::CloseApp {
            process: "notepad.exe".into(),
        };
        let open = Fake {
            running: vec!["notepad.exe".into()],
            ..Fake::default()
        };
        assert_eq!(run(&close, &open), Ok(Outcome::Done));
        assert_eq!(calls(&open), ["close notepad.exe"]);
        // Не запущена — не ошибка: закрывать нечего.
        let none = Fake::default();
        assert_eq!(run(&close, &none), Ok(Outcome::Done));
        // Сбой системного вызова — ошибка действия («Не удалось, сэр»).
        let broken = Fake {
            fail: true,
            ..Fake::default()
        };
        assert!(matches!(run(&close, &broken), Err(ActionError::Close(_))));
    }

    #[test]
    fn dictate_note_hands_target_to_core() {
        let action = ActionSpec::DictateNote {
            url: " https://keep.google.com/#NOTE/abc ".into(),
            title: "Идеи".into(),
            prefix: "- ".into(),
        };
        let fake = Fake::default();
        assert_eq!(
            run(&action, &fake),
            Ok(Outcome::Dictate(NoteTarget {
                url: "https://keep.google.com/#NOTE/abc".into(),
                title: "Идеи".into(),
                prefix: "- ".into(),
            }))
        );
        // Ничего системного действие само не делает — открывает и пишет ядро.
        assert!(calls(&fake).is_empty());
    }
}
