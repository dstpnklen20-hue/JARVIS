//! Звуковой пакет голоса: `sounds/<пакет>/pack.toml` + WAV-файлы.
//!
//! ```toml
//! name = "Стандартный"
//! [replies]
//! wake = ["wake_1.wav", "wake_2.wav"]   # обязательно
//! ok = ["ok_1.wav"]
//! ```
//!
//! На каждый ответ можно положить несколько вариантов — Джарвис чередует их.
//!
//! Необязательные приветствия при запуске — по времени суток:
//!
//! ```toml
//! [greetings]
//! morning = ["greeting_1.wav"]   # 5–11 ч
//! day = ["greeting_2.wav"]       # 12–16 ч
//! evening = ["greeting_3.wav"]   # 17–22 ч
//! night = ["greeting_4.wav"]     # 23–4 ч; нет — звучит вечернее
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use jarvis_core::state::Reply;
use serde::Deserialize;

use crate::AudioError;
use crate::wav::{self, Clip};

pub const PACK_FILE: &str = "pack.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackFile {
    name: String,
    replies: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    greetings: BTreeMap<String, Vec<String>>,
}

/// Часть суток — для приветствия при запуске.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DayPart {
    Morning,
    Day,
    Evening,
    Night,
}

impl DayPart {
    pub const ALL: [DayPart; 4] = [Self::Morning, Self::Day, Self::Evening, Self::Night];

    pub fn from_hour(hour: u8) -> Self {
        match hour {
            5..=11 => Self::Morning,
            12..=16 => Self::Day,
            17..=22 => Self::Evening,
            _ => Self::Night,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Morning => "morning",
            Self::Day => "day",
            Self::Evening => "evening",
            Self::Night => "night",
        }
    }
}

/// Пакет, загруженный в память при старте (ответ звучит без задержки на диск).
#[derive(Debug, Clone)]
pub struct VoicePack {
    pub name: String,
    pub dir: PathBuf,
    clips: HashMap<Reply, Vec<Clip>>,
    greetings: HashMap<DayPart, Vec<Clip>>,
}

impl VoicePack {
    pub fn load(dir: &Path) -> Result<Self, AudioError> {
        let path = dir.join(PACK_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| AudioError::Read {
            path: path.clone(),
            source,
        })?;
        let file: PackFile = toml::from_str(&text).map_err(|source| AudioError::PackToml {
            path: path.clone(),
            source,
        })?;
        let bad = |reason: String| AudioError::Pack {
            path: path.clone(),
            reason,
        };

        let mut clips = HashMap::new();
        for (key, files) in file.replies {
            let reply = Reply::ALL
                .into_iter()
                .find(|r| r.key() == key)
                .ok_or_else(|| {
                    let known: Vec<_> = Reply::ALL.iter().map(|r| r.key()).collect();
                    bad(format!(
                        "неизвестный ответ `{key}`; допустимые: {}",
                        known.join(", ")
                    ))
                })?;
            if files.is_empty() {
                return Err(bad(format!("у ответа `{key}` нет ни одного файла")));
            }
            let loaded = files
                .iter()
                .map(|f| wav::read(&dir.join(f)))
                .collect::<Result<Vec<_>, _>>()?;
            clips.insert(reply, loaded);
        }
        if !clips.contains_key(&Reply::Wake) {
            return Err(bad(format!(
                "нет обязательного ответа `{}`",
                Reply::Wake.key()
            )));
        }
        let mut greetings = HashMap::new();
        for (key, files) in file.greetings {
            let part = DayPart::ALL
                .into_iter()
                .find(|p| p.key() == key)
                .ok_or_else(|| {
                    bad(format!(
                        "неизвестная часть суток `{key}` в [greetings]; допустимые: morning, day, evening, night"
                    ))
                })?;
            let loaded = files
                .iter()
                .map(|f| wav::read(&dir.join(f)))
                .collect::<Result<Vec<_>, _>>()?;
            greetings.insert(part, loaded);
        }
        Ok(Self {
            name: file.name,
            dir: dir.to_path_buf(),
            clips,
            greetings,
        })
    }

    pub fn variants(&self, reply: Reply) -> &[Clip] {
        self.clips.get(&reply).map_or(&[], Vec::as_slice)
    }

    /// Приветствия для части суток; ночью без своих — вечерние.
    pub fn greetings(&self, part: DayPart) -> &[Clip] {
        let own = self.greetings.get(&part).map_or(&[][..], Vec::as_slice);
        if own.is_empty() && part == DayPart::Night {
            return self.greetings(DayPart::Evening);
        }
        own
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_clip(dir: &Path, name: &str) {
        let clip = Clip {
            samples: vec![0.1; 1600],
            sample_rate: 16_000,
        };
        wav::write(&dir.join(name), &clip).unwrap();
    }

    fn load_pack(toml: &str, wavs: &[&str]) -> (tempfile::TempDir, Result<VoicePack, AudioError>) {
        let dir = tempfile::tempdir().unwrap();
        for w in wavs {
            write_clip(dir.path(), w);
        }
        std::fs::write(dir.path().join(PACK_FILE), toml).unwrap();
        let result = VoicePack::load(dir.path());
        (dir, result)
    }

    #[test]
    fn loads_valid_pack() {
        let (_dir, pack) = load_pack(
            "name = \"Тест\"\n[replies]\nwake = [\"a.wav\", \"b.wav\"]\nfailed = [\"a.wav\"]\n",
            &["a.wav", "b.wav"],
        );
        let pack = pack.unwrap();
        assert_eq!(pack.name, "Тест");
        assert_eq!(pack.variants(Reply::Wake).len(), 2);
        assert_eq!(pack.variants(Reply::Failed).len(), 1);
        assert!(pack.variants(Reply::Done).is_empty());
        assert_eq!(pack.variants(Reply::Wake)[0].sample_rate, 16_000);
    }

    #[test]
    fn greetings_by_part_of_day() {
        let (_dir, pack) = load_pack(
            "name = \"x\"\n[replies]\nwake = [\"a.wav\"]\n[greetings]\nmorning = [\"a.wav\"]\nevening = [\"a.wav\", \"b.wav\"]\n",
            &["a.wav", "b.wav"],
        );
        let pack = pack.unwrap();
        assert_eq!(pack.greetings(DayPart::Morning).len(), 1);
        assert!(pack.greetings(DayPart::Day).is_empty());
        assert_eq!(pack.greetings(DayPart::Night).len(), 2, "ночью — вечерние");
        assert_eq!(DayPart::from_hour(7), DayPart::Morning);
        assert_eq!(DayPart::from_hour(12), DayPart::Day);
        assert_eq!(DayPart::from_hour(22), DayPart::Evening);
        assert_eq!(DayPart::from_hour(3), DayPart::Night);

        let (_dir, bad) = load_pack(
            "name = \"x\"\n[replies]\nwake = [\"a.wav\"]\n[greetings]\nnoon = [\"a.wav\"]\n",
            &["a.wav"],
        );
        assert!(bad.unwrap_err().to_string().contains("noon"));
    }

    #[test]
    fn unknown_reply_is_rejected() {
        let (_dir, pack) = load_pack(
            "name = \"x\"\n[replies]\nwake = [\"a.wav\"]\nwaek = [\"a.wav\"]\n",
            &["a.wav"],
        );
        let err = pack.unwrap_err().to_string();
        assert!(
            err.contains("waek") && err.contains("not_understood"),
            "{err}"
        );
    }

    #[test]
    fn missing_wake_is_rejected() {
        let (_dir, pack) = load_pack("name = \"x\"\n[replies]\nok = [\"a.wav\"]\n", &["a.wav"]);
        assert!(pack.unwrap_err().to_string().contains("wake"));
    }

    #[test]
    fn missing_file_and_unknown_field_are_rejected() {
        let (_dir, pack) = load_pack("name = \"x\"\n[replies]\nwake = [\"нет.wav\"]\n", &[]);
        assert!(pack.unwrap_err().to_string().contains("нет.wav"));
        let (_dir, pack) = load_pack(
            "name = \"x\"\nvoice = 1\n[replies]\nwake = [\"a.wav\"]\n",
            &["a.wav"],
        );
        assert!(matches!(pack.unwrap_err(), AudioError::PackToml { .. }));
    }

    #[test]
    fn empty_variant_list_is_rejected() {
        let (_dir, pack) = load_pack("name = \"x\"\n[replies]\nwake = []\n", &[]);
        assert!(
            pack.unwrap_err()
                .to_string()
                .contains("нет ни одного файла")
        );
    }
}
