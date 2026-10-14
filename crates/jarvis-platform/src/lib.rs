//! Платформозависимый код. Всё, что касается WinAPI, живёт здесь, чтобы
//! остальные крейты оставались переносимыми и без `unsafe`.

use std::fmt;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
use unsupported as imp;

/// Цикл сообщений главного потока и обработка Ctrl+C / закрытия консоли.
#[cfg(windows)]
pub use windows::{
    LoopEvent, Waker, current_waker, install_ctrl_handler, notify_shutdown_complete,
    run_message_loop,
};

/// Глобальные горячие клавиши (аварийная остановка).
#[cfg(windows)]
pub use windows::{HotkeyGuard, parse_hotkey, register_hotkey};

/// Окно-вопрос с кнопками (вопрос-разрешение).
#[cfg(windows)]
pub use windows::{DialogButton, DialogHandle, DialogSpec, show_dialog};

/// Автозапуск при входе в Windows (HKCU\...\Run).
#[cfg(windows)]
pub use windows::{autostart_command, set_autostart};

/// Короткое (латинское) имя пути для нативных библиотек.
#[cfg(windows)]
pub use windows::short_path;

/// Окно с текстом ответа нейросети.
#[cfg(windows)]
pub use windows::{AnswerStatus, AnswerView, AnswerWindow, LogoPainter, show_answer_window};

#[cfg(windows)]
pub use windows::{
    AppWindow, AppWindowSpec, ProcessUsage, open_app_window, process_usage, use_per_monitor_dpi,
};

/// Системные действия команд: адреса, мультимедиа, процессы, клавиши, питание, громкость.
#[cfg(windows)]
pub use windows::{
    MediaKey, Power, RunEnd, activate_browser_tab, close_app, focus_app, focus_browser, media_key,
    open_url, power, press_keys, process_running, release_all_keys, run_and_wait, set_volume,
    spawn_detached,
};

/// Мягкая остановка работающей копии извне (`jarvis --stop`).
#[cfg(windows)]
pub use windows::{
    StopSignal, create_show_signal, create_stop_signal, send_show_signal, send_stop_signal,
};

/// Пока жив, вторая копия программы не запустится.
pub struct InstanceGuard {
    _handle: Box<dyn Send>,
}

/// Захватывает «право быть единственной копией» с именем `name`.
/// `Ok(None)` — другая копия уже запущена.
pub fn acquire_single_instance(name: &str) -> Result<Option<InstanceGuard>, PlatformError> {
    imp::acquire_single_instance(name)
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("{call} завершился с ошибкой Win32 {code}")]
    Win32 { call: &'static str, code: u32 },
    #[error("не поддерживается на этой платформе")]
    Unsupported,
}

/// Версия операционной системы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    /// Например, «Windows 11 Pro».
    pub product: String,
    /// Например, «24H2». Может быть пустым на старых сборках.
    pub display_version: String,
    pub build: u32,
    /// Номер накопительного обновления (Update Build Revision).
    pub revision: u32,
}

impl fmt::Display for OsInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.product)?;
        if !self.display_version.is_empty() {
            write!(f, " {}", self.display_version)?;
        }
        write!(f, " (сборка {}.{})", self.build, self.revision)
    }
}

/// Сборки Windows начиная с этой — Windows 11.
pub const WINDOWS_11_FIRST_BUILD: u32 = 22000;

/// Минимальная поддерживаемая сборка: Windows 10 1809.
pub const MIN_SUPPORTED_BUILD: u32 = 17763;

pub fn os_info() -> Result<OsInfo, PlatformError> {
    imp::os_info()
}

/// Запущен ли процесс с правами администратора (повышенный токен).
pub fn is_elevated() -> Result<bool, PlatformError> {
    imp::is_elevated()
}

/// Реестр Windows 11 по историческим причинам пишет в `ProductName`
/// «Windows 10». Исправляем по номеру сборки.
fn fix_product_name(product: &str, build: u32) -> String {
    if build >= WINDOWS_11_FIRST_BUILD {
        product.replacen("Windows 10", "Windows 11", 1)
    } else {
        product.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_name_is_fixed_for_windows_11() {
        assert_eq!(fix_product_name("Windows 10 Pro", 26100), "Windows 11 Pro");
        assert_eq!(fix_product_name("Windows 10 Pro", 19045), "Windows 10 Pro");
        assert_eq!(
            fix_product_name("Windows 11 Home", 26100),
            "Windows 11 Home"
        );
    }

    #[test]
    fn display_formats_all_parts() {
        let info = OsInfo {
            product: "Windows 11 Pro".into(),
            display_version: "24H2".into(),
            build: 26100,
            revision: 4061,
        };
        assert_eq!(info.to_string(), "Windows 11 Pro 24H2 (сборка 26100.4061)");
    }

    #[cfg(windows)]
    #[test]
    fn real_os_info_is_readable() {
        let info = os_info().unwrap();
        assert!(info.build >= MIN_SUPPORTED_BUILD, "{info}");
        assert!(info.product.starts_with("Windows"), "{info}");
    }

    #[cfg(windows)]
    #[test]
    fn elevation_check_does_not_fail() {
        is_elevated().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn second_instance_is_detected() {
        let name = format!("JarvisTest-{}", std::process::id());
        let first = acquire_single_instance(&name).unwrap();
        assert!(first.is_some());
        assert!(acquire_single_instance(&name).unwrap().is_none());
        drop(first);
        // После освобождения можно снова.
        assert!(acquire_single_instance(&name).unwrap().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn stop_signal_reaches_waiting_thread() {
        let name = format!("JarvisStopTest-{}", std::process::id());
        assert!(!send_stop_signal(&name).unwrap(), "без получателя — false");
        let signal = create_stop_signal(&name).unwrap();
        let waiter = std::thread::spawn(move || signal.wait());
        assert!(send_stop_signal(&name).unwrap());
        waiter.join().unwrap().unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "на полсекунды показывает окно"]
    fn dialog_can_be_closed_from_outside() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut handle = show_dialog(
            DialogSpec {
                title: "Джарвис — тест".into(),
                heading: "Проверка окна".into(),
                text: "Окно закроется само.".into(),
                buttons: vec![
                    DialogButton {
                        id: 100,
                        label: "Да".into(),
                    },
                    DialogButton {
                        id: 101,
                        label: "Нет".into(),
                    },
                ],
                default_button: 101,
            },
            move |result| {
                let _ = tx.send(result);
            },
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        handle.close();
        let result = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(result, None, "закрыто извне — без нажатой кнопки");
    }

    #[cfg(windows)]
    #[test]
    fn waker_from_other_thread_ends_loop() {
        let waker = current_waker();
        let mut ticks = 0;
        std::thread::spawn(move || {
            waker.wake();
            waker.quit();
        });
        run_message_loop(std::time::Duration::from_secs(5), |_| {
            ticks += 1;
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
        // Пробуждение вызвало обработчик, quit завершил цикл.
        assert!(ticks >= 1);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "на 3 секунды показывает окно в углу экрана"]
    fn answer_window_shows_updates_and_closes() {
        let view = |answer: &str, status| AnswerView {
            question: "кто такой Тони Старк?".into(),
            answer: answer.into(),
            status,
        };
        let window = show_answer_window(view("", AnswerStatus::Thinking), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(window.is_open());
        window.set(view(
            "Тони Старк — гениальный изобретатель и глава Stark Industries, сэр.",
            AnswerStatus::Answering,
        ));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        window.set_status(AnswerStatus::Done);
        std::thread::sleep(std::time::Duration::from_millis(1000));
        window.close();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!window.is_open());
    }

    /// Открывает Блокнот и Калькулятор (приложение из Microsoft Store — окно у
    /// ApplicationFrameHost) и закрывает их, как крестиком. Закроет и уже
    /// открытые окна этих программ — запускать, когда они не нужны.
    #[cfg(windows)]
    #[test]
    #[ignore = "открывает и закрывает Блокнот и Калькулятор"]
    fn close_app_closes_classic_and_store_windows() {
        use std::time::{Duration, Instant};
        let wait_window = |process: &str| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                if focus_app(process).unwrap() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            false
        };
        for (program, process) in [
            ("notepad.exe", "notepad.exe"),
            ("calc.exe", "CalculatorApp.exe"),
        ] {
            spawn_detached(std::path::Path::new(program), &[], None).unwrap();
            assert!(wait_window(process), "{program}: окно не появилось");
            assert!(
                close_app(process).unwrap() >= 1,
                "{process}: окна не нашлись"
            );
            std::thread::sleep(Duration::from_secs(2));
            assert_eq!(
                close_app(process).unwrap(),
                0,
                "{process}: окно не закрылось"
            );
        }
        // Не запущена — закрывать нечего.
        assert_eq!(close_app("nosuchprogram.exe").unwrap(), 0);
    }
}
