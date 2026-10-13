//! Слух и голос Джарвиса: захват звука, слово активации, распознавание
//! говорящего, детектор речи, звуки ответа.
//!
//! Все нейросети — sherpa-onnx (раздел 4 ТЗ, выбор обоснован в отчёте этапа 1).

pub mod asr;
pub mod bench;
pub mod bpe;
pub mod capture;
pub mod engines;
mod error;
pub mod level;
pub mod models;
pub mod pack;
pub mod pipeline;
pub mod player;
pub mod resample;
pub mod speaker;
pub mod stt;
pub mod tts;
pub mod vad;
pub mod wake;
pub mod wav;

pub use error::AudioError;
