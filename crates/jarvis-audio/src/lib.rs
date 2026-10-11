//! Слух и голос Джарвиса: захват звука, слово активации, распознавание
//! говорящего, детектор речи, звуки ответа.
//!
//! Все нейросети — sherpa-onnx (раздел 4 ТЗ, выбор обоснован в отчёте этапа 1).

pub mod asr;
pub mod bench;
pub mod capture;
mod error;
pub mod models;
pub mod pack;
pub mod pipeline;
pub mod player;
pub mod resample;
pub mod speaker;
pub mod vad;
pub mod wake;
pub mod wav;

pub use error::AudioError;
