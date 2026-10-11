//! Цикл сообщений Windows для главного потока (нужен трею) и пробуждение
//! этого потока из других потоков.
//!
//! Пробуждение — `PostThreadMessageW`. Такие сообщения теряются, пока поток
//! крутит чужой модальный цикл (например, открыто меню трея), поэтому цикл
//! дополнительно просыпается по таймеру: обработчик вызывается не реже чем
//! раз в `tick_ms`, и всё накопленное в каналах будет обработано.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Console::{
    CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    SetTimer, TranslateMessage, WM_APP, WM_USER,
};
use windows_sys::core::BOOL;

use super::last_error;
use crate::PlatformError;

const WM_JARVIS_WAKE: u32 = WM_APP + 1;
const WM_JARVIS_QUIT: u32 = WM_APP + 2;

/// Будит цикл сообщений потока, создавшего этот `Waker`. Можно копировать
/// и передавать в любые потоки.
#[derive(Debug, Clone, Copy)]
pub struct Waker {
    thread_id: u32,
}

impl Waker {
    /// Попросить цикл вызвать обработчик как можно скорее.
    pub fn wake(&self) {
        self.post(WM_JARVIS_WAKE);
    }

    /// Попросить цикл завершиться.
    pub fn quit(&self) {
        self.post(WM_JARVIS_QUIT);
    }

    fn post(&self, message: u32) {
        // SAFETY: PostThreadMessageW только ставит сообщение в очередь; при
        // ошибке (поток уже завершился) просто возвращает 0 — нас это устраивает.
        unsafe {
            PostThreadMessageW(self.thread_id, message, 0, 0);
        }
    }
}

/// `Waker` для текущего потока. Вызывайте в том потоке, где будет
/// `run_message_loop`, до запуска других потоков.
pub fn current_waker() -> Waker {
    let mut msg = empty_msg();
    // SAFETY: PeekMessageW с PM_NOREMOVE ничего не извлекает; вызов нужен,
    // чтобы у потока появилась очередь сообщений — без неё PostThreadMessageW
    // из других потоков завершился бы ошибкой.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        );
    }
    Waker {
        // SAFETY: без аргументов и побочных эффектов.
        thread_id: unsafe { GetCurrentThreadId() },
    }
}

/// Крутит цикл сообщений текущего потока. `on_tick` вызывается после каждого
/// сообщения, по `Waker::wake` и по таймеру; `Break` завершает цикл.
pub fn run_message_loop(
    tick: Duration,
    mut on_tick: impl FnMut() -> ControlFlow<()>,
) -> Result<(), PlatformError> {
    let tick_ms = u32::try_from(tick.as_millis()).unwrap_or(u32::MAX).max(10);
    // SAFETY: таймер потока (без окна и без callback): приходит как WM_TIMER
    // в очередь этого потока.
    let timer = unsafe { SetTimer(std::ptr::null_mut(), 0, tick_ms, None) };
    if timer == 0 {
        return Err(last_error("SetTimer"));
    }

    let mut msg = empty_msg();
    let result = loop {
        // SAFETY: `msg` — валидный буфер; null-окно — сообщения всего потока.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        match got {
            -1 => break Err(last_error("GetMessageW")),
            0 => break Ok(()), // WM_QUIT
            _ => {}
        }
        match msg.message {
            WM_JARVIS_QUIT => break Ok(()),
            WM_JARVIS_WAKE => {}
            _ => {
                // SAFETY: сообщение только что получено GetMessageW.
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        if on_tick().is_break() {
            break Ok(());
        }
    };

    // SAFETY: таймер создан выше этим же потоком.
    unsafe {
        KillTimer(std::ptr::null_mut(), timer);
    }
    result
}

fn empty_msg() -> MSG {
    MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
    }
}

static CTRL_TARGET_THREAD: AtomicU32 = AtomicU32::new(0);
static SHUTDOWN_COMPLETE: AtomicBool = AtomicBool::new(false);

/// Сколько обработчик закрытия консоли ждёт корректного завершения.
/// Windows даёт около 5 секунд, потом завершает процесс сама.
const CLOSE_WAIT: Duration = Duration::from_secs(4);

/// Ctrl+C, Ctrl+Break, закрытие окна консоли, выход из системы →
/// `waker.quit()`. При закрытии окна ждём [`notify_shutdown_complete`],
/// чтобы успеть освободить микрофон и дописать логи.
pub fn install_ctrl_handler(waker: Waker) -> Result<(), PlatformError> {
    CTRL_TARGET_THREAD.store(waker.thread_id, Ordering::SeqCst);
    // SAFETY: регистрируем функцию с правильной сигнатурой; она живёт
    // всё время работы процесса.
    let ok = unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), 1) };
    if ok == 0 {
        return Err(last_error("SetConsoleCtrlHandler"));
    }
    Ok(())
}

/// Сообщить обработчику консоли, что корректное завершение выполнено.
pub fn notify_shutdown_complete() {
    SHUTDOWN_COMPLETE.store(true, Ordering::SeqCst);
}

extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    let thread_id = CTRL_TARGET_THREAD.load(Ordering::SeqCst);
    Waker { thread_id }.quit();
    if matches!(
        ctrl_type,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        // После возврата из обработчика Windows завершит процесс сразу.
        let start = Instant::now();
        while !SHUTDOWN_COMPLETE.load(Ordering::SeqCst) && start.elapsed() < CLOSE_WAIT {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    1
}
