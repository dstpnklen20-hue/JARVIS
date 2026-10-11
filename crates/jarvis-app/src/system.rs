//! Связка исполнителей действий с системой: реализация
//! `jarvis_actions::System` через `jarvis_platform`.

use jarvis_actions::System;
use jarvis_core::commands::MediaOp;

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
        };
        jarvis_platform::media_key(key, presses)
    }
}
