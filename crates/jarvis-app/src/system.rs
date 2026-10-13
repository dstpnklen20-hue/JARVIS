//! Связка исполнителей действий с системой: реализация
//! `jarvis_actions::System` через `jarvis_platform`.

use std::time::Duration;

use jarvis_actions::{CancelToken, ProcessEnd, ProcessResult, ProcessSpec, System};
use jarvis_core::commands::{MediaOp, PowerOp};

pub struct PlatformSystem;

impl System for PlatformSystem {
    fn open_url(&self, url: &str) -> Result<(), String> {
        jarvis_platform::open_url(url)
    }

    fn media_key(&self, op: MediaOp, presses: u32) -> Result<(), String> {
        use jarvis_platform::MediaKey as K;
        let key = match op {
            MediaOp::PlayPause => K::PlayPause,
            MediaOp::Next => K::Next,
            MediaOp::Previous => K::Previous,
            MediaOp::Stop => K::Stop,
            MediaOp::VolumeUp => K::VolumeUp,
            MediaOp::VolumeDown => K::VolumeDown,
            MediaOp::Mute => K::Mute,
            MediaOp::VolumeSet => return Err("volume_set — это не клавиша".into()),
        };
        jarvis_platform::media_key(key, presses)
    }

    fn set_volume(&self, percent: u32) -> Result<(), String> {
        jarvis_platform::set_volume(percent)
    }

    fn spawn_detached(&self, spec: &ProcessSpec) -> Result<(), String> {
        jarvis_platform::spawn_detached(&spec.program, &spec.args, spec.working_dir.as_deref())
    }

    fn run_process(
        &self,
        spec: &ProcessSpec,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<ProcessResult, String> {
        let (end, output) = jarvis_platform::run_and_wait(
            &spec.program,
            &spec.args,
            spec.working_dir.as_deref(),
            spec.hide_window,
            timeout,
            cancel.flag(),
        )?;
        let end = match end {
            jarvis_platform::RunEnd::Exited(code) => ProcessEnd::Exited(code),
            jarvis_platform::RunEnd::TimedOut => ProcessEnd::TimedOut,
            jarvis_platform::RunEnd::Cancelled => ProcessEnd::Cancelled,
        };
        Ok(ProcessResult { end, output })
    }

    fn press_keys(&self, vks: &[u16], hold: Duration) -> Result<(), String> {
        jarvis_platform::press_keys(vks, hold)
    }

    fn power(&self, op: PowerOp) -> Result<(), String> {
        use jarvis_platform::Power as P;
        jarvis_platform::power(match op {
            PowerOp::Lock => P::Lock,
            PowerOp::Sleep => P::Sleep,
            PowerOp::Logoff => P::Logoff,
            PowerOp::Restart => P::Restart,
            PowerOp::Shutdown => P::Shutdown,
        })
    }

    fn activate_tab(&self, matches: &dyn Fn(&str) -> bool) -> Result<Option<String>, String> {
        jarvis_platform::activate_browser_tab(matches)
    }

    fn process_running(&self, name: &str) -> Result<bool, String> {
        jarvis_platform::process_running(name)
    }
}
