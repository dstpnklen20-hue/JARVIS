//! Слоты: параметры команды из речи (раздел 2.5 ТЗ).
//!
//! «громкость на {level}» — `level` описан в манифесте с типом и проверкой:
//! число в диапазоне или значение из списка. Так ослышка не превратится в
//! непредсказуемую команду. Значения подставляются только в аргументы,
//! адреса и уровень громкости — никогда в путь к программе или скрипту.
//!
//! ```toml
//! [command.slots.level]
//! type = "number"
//! min = 0
//! max = 100
//!
//! [command.slots.site]
//! type = "choice"
//! values = { "ютуб" = "youtube.com", "гугл" = "google.com" }   # или ["ютуб", "гугл"]
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Значения слотов, распознанные во фразе: имя → значение для подстановки.
pub type SlotValues = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SlotSpec {
    Number { min: i64, max: i64 },
    Choice { values: ChoiceValues },
}

/// Список («как сказано» = что подставить) или карта «как сказано → что подставить».
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ChoiceValues {
    List(Vec<String>),
    Map(BTreeMap<String, String>),
}

impl ChoiceValues {
    /// Как значения произносятся.
    pub fn spoken(&self) -> Vec<&str> {
        match self {
            ChoiceValues::List(list) => list.iter().map(String::as_str).collect(),
            ChoiceValues::Map(map) => map.keys().map(String::as_str).collect(),
        }
    }

    fn value_for(&self, spoken: &str) -> Option<String> {
        match self {
            ChoiceValues::List(list) => list.iter().find(|v| v.as_str() == spoken).cloned(),
            ChoiceValues::Map(map) => map.get(spoken).cloned(),
        }
    }
}

impl SlotSpec {
    pub fn validate(&self, name: &str) -> Result<(), String> {
        if !valid_name(name) {
            return Err(format!(
                "слот `{name}`: имя — латиница в нижнем регистре, цифры и «_»"
            ));
        }
        match self {
            SlotSpec::Number { min, max } if min > max => {
                Err(format!("слот `{name}`: min ({min}) больше max ({max})"))
            }
            SlotSpec::Number { .. } => Ok(()),
            SlotSpec::Choice { values } => {
                let spoken = values.spoken();
                if spoken.is_empty() {
                    return Err(format!("слот `{name}`: пустой список значений"));
                }
                if let Some(bad) = spoken.iter().find(|s| jarvis_nlu::normalize(s).is_empty()) {
                    return Err(format!(
                        "слот `{name}`: значение «{bad}» после нормализации пустое"
                    ));
                }
                if let ChoiceValues::Map(map) = values
                    && let Some((k, _)) = map.iter().find(|(_, v)| v.trim().is_empty())
                {
                    return Err(format!(
                        "слот `{name}`: у значения «{k}» пустая подстановка"
                    ));
                }
                Ok(())
            }
        }
    }

    /// Проверяет распознанное значение и возвращает то, что подставить.
    /// Для числа `raw` — цифры; для выбора — произнесённое значение как в манифесте.
    pub fn accept(&self, raw: &str) -> Option<String> {
        match self {
            SlotSpec::Number { min, max } => {
                let n: i64 = raw.trim().parse().ok()?;
                (*min..=*max).contains(&n).then(|| n.to_string())
            }
            SlotSpec::Choice { values } => values.value_for(raw),
        }
    }
}

/// Имя слота: `[a-z_][a-z0-9_]*`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Имена слотов, упомянутых в строке как `{имя}`.
pub fn placeholders(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                out.push(&after[..end]);
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// Подставляет значения слотов в строку.
pub fn fill(s: &str, values: &SlotValues) -> String {
    values.iter().fold(s.to_owned(), |acc, (name, value)| {
        acc.replace(&format!("{{{name}}}"), value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(min: i64, max: i64) -> SlotSpec {
        SlotSpec::Number { min, max }
    }

    #[test]
    fn number_slot_accepts_only_range() {
        let s = number(0, 100);
        assert_eq!(s.accept("40"), Some("40".into()));
        assert_eq!(s.accept("0"), Some("0".into()));
        assert_eq!(s.accept("101"), None);
        assert_eq!(s.accept("-5"), None);
        assert_eq!(s.accept("сорок"), None);
        assert!(number(5, 1).validate("x").is_err());
    }

    #[test]
    fn choice_slot_list_and_map() {
        let list = SlotSpec::Choice {
            values: ChoiceValues::List(vec!["браузер".into(), "плеер".into()]),
        };
        assert_eq!(list.accept("плеер"), Some("плеер".into()));
        assert_eq!(list.accept("чайник"), None);

        let map: BTreeMap<String, String> = [("ютуб".to_owned(), "youtube.com".to_owned())]
            .into_iter()
            .collect();
        let map = SlotSpec::Choice {
            values: ChoiceValues::Map(map),
        };
        assert_eq!(map.accept("ютуб"), Some("youtube.com".into()));
    }

    #[test]
    fn choice_slot_parses_from_toml_both_ways() {
        #[derive(Deserialize)]
        struct W {
            s: SlotSpec,
        }
        let list: W = toml::from_str("[s]\ntype = \"choice\"\nvalues = [\"а\", \"б\"]\n").unwrap();
        assert!(matches!(
            list.s,
            SlotSpec::Choice {
                values: ChoiceValues::List(_)
            }
        ));
        let map: W =
            toml::from_str("[s]\ntype = \"choice\"\nvalues = { \"ютуб\" = \"y.com\" }\n").unwrap();
        assert!(matches!(
            map.s,
            SlotSpec::Choice {
                values: ChoiceValues::Map(_)
            }
        ));
    }

    #[test]
    fn slot_names_and_bad_choices() {
        assert!(valid_name("level"));
        assert!(valid_name("_x2"));
        assert!(!valid_name("Level"));
        assert!(!valid_name("2x"));
        assert!(!valid_name("уровень"));
        let empty = SlotSpec::Choice {
            values: ChoiceValues::List(vec![]),
        };
        assert!(empty.validate("x").is_err());
    }

    #[test]
    fn placeholders_and_fill() {
        assert_eq!(placeholders("громкость на {level}"), ["level"]);
        assert_eq!(placeholders("{a} и {b}"), ["a", "b"]);
        assert!(placeholders("без слотов").is_empty());
        assert!(placeholders("сломан {").is_empty());
        let values: SlotValues = [("level".to_owned(), "40".to_owned())]
            .into_iter()
            .collect();
        assert_eq!(fill("--volume={level}", &values), "--volume=40");
        assert_eq!(fill("{other}", &values), "{other}");
    }
}
