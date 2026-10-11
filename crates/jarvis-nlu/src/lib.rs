//! Понимание команд (раздел 4.4 ТЗ): нормализация распознанного текста и
//! нечёткое сопоставление с фразами зарегистрированных команд.
//!
//! Чистые функции без ввода-вывода — всё проверяется тестами.

pub mod answers;
pub mod matcher;
pub mod normalize;
pub mod numbers;

pub use matcher::{CommandMatcher, CommandPhrases, MatchResult, SlotKind, Thresholds};
pub use normalize::normalize;
