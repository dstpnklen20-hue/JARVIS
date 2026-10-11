//! Сборка движков слуха так, как они работают в демоне: один распознаватель
//! (один экземпляр модели в памяти) и два потока на нём — для слова
//! активации и для команд. Используется демоном, `--bench` и тестами, чтобы
//! замеры шли ровно по рабочей конфигурации.

use std::sync::Arc;

use crate::AudioError;
use crate::asr::{Hotwords, create_recognizer_with};
use crate::models::AsrModel;
use crate::stt::{SherpaStt, command_endpointing};
use crate::wake::{SherpaWakeWord, WakeMatcher};

pub struct Engines {
    pub wake: SherpaWakeWord,
    pub stt: SherpaStt,
}

/// `end_silence` — тишина в конце команды, секунды.
pub fn build(
    model: &AsrModel,
    matcher: WakeMatcher,
    end_silence: f32,
    hotwords: Option<&Hotwords>,
) -> Result<Engines, AudioError> {
    let recognizer = Arc::new(create_recognizer_with(
        model,
        command_endpointing(end_silence),
        hotwords,
    )?);
    Ok(Engines {
        wake: SherpaWakeWord::with_recognizer(Arc::clone(&recognizer), matcher),
        stt: SherpaStt::with_recognizer(recognizer),
    })
}
