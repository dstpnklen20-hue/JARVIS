//! Словарь BPE для подсказок (hotwords) распознавателю.
//!
//! sherpa-onnx умеет сам разбивать подсказки на токены, если дать ему
//! `bpe.vocab` — список «кусок \t вес». В архиве модели есть только
//! `bpe.model` (protobuf SentencePiece), поэтому выгружаем словарь сами:
//! без Python и без лишних зависимостей — формат простой.
//!
//! ModelProto: поле 1 (повторяется) — SentencePiece { 1: piece (строка),
//! 2: score (float32), 3: type (varint) }. Прочие поля пропускаются.

use std::path::{Path, PathBuf};

use crate::AudioError;

#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub text: String,
    pub score: f32,
}

/// Разбирает `bpe.model`.
pub fn parse_model(bytes: &[u8]) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut r = Reader::new(bytes);
    while !r.done() {
        let (field, wire) = r.key()?;
        if field == 1 && wire == WIRE_LEN {
            pieces.push(parse_piece(r.bytes()?)?);
        } else {
            r.skip(wire)?;
        }
    }
    if pieces.is_empty() {
        return Err("в модели нет ни одного токена".into());
    }
    Ok(pieces)
}

fn parse_piece(bytes: &[u8]) -> Result<Piece, String> {
    let mut r = Reader::new(bytes);
    let mut text = None;
    let mut score = 0.0;
    while !r.done() {
        match r.key()? {
            (1, WIRE_LEN) => {
                text = Some(String::from_utf8(r.bytes()?.to_vec()).map_err(|e| e.to_string())?);
            }
            (2, WIRE_FIXED32) => score = f32::from_le_bytes(r.fixed32()?),
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(Piece {
        text: text.ok_or("токен без текста")?,
        score,
    })
}

/// Текст `bpe.vocab` в формате sherpa-onnx.
pub fn vocab_text(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| format!("{}\t{}\n", p.text, p.score))
        .collect()
}

/// Создаёт `bpe.vocab` рядом с `bpe.model`, если его нет; возвращает путь.
pub fn ensure_vocab(model: &Path) -> Result<PathBuf, AudioError> {
    let vocab = model.with_extension("vocab");
    if vocab.is_file() {
        return Ok(vocab);
    }
    let bytes = std::fs::read(model).map_err(|source| AudioError::Read {
        path: model.to_path_buf(),
        source,
    })?;
    let pieces = parse_model(&bytes).map_err(|reason| AudioError::Wav {
        path: model.to_path_buf(),
        reason: format!("не удалось разобрать BPE-модель: {reason}"),
    })?;
    // Пишем через временный файл: оборванная запись не оставит битый словарь.
    let tmp = vocab.with_extension("vocab.tmp");
    std::fs::write(&tmp, vocab_text(&pieces)).map_err(|source| AudioError::Write {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, &vocab).map_err(|source| AudioError::Write {
        path: vocab.clone(),
        source,
    })?;
    Ok(vocab)
}

const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_LEN: u8 = 2;
const WIRE_FIXED32: u8 = 5;

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.data.get(self.pos).ok_or("обрыв varint")?;
            self.pos += 1;
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("слишком длинный varint".into())
    }

    fn key(&mut self) -> Result<(u64, u8), String> {
        let key = self.varint()?;
        Ok((key >> 3, (key & 7) as u8))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("переполнение длины")?;
        let slice = self.data.get(self.pos..end).ok_or("обрыв данных")?;
        self.pos = end;
        Ok(slice)
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = usize::try_from(self.varint()?).map_err(|e| e.to_string())?;
        self.take(len)
    }

    fn fixed32(&mut self) -> Result<[u8; 4], String> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    fn skip(&mut self, wire: u8) -> Result<(), String> {
        match wire {
            WIRE_VARINT => self.varint().map(drop),
            WIRE_FIXED64 => self.take(8).map(drop),
            WIRE_LEN => self.bytes().map(drop),
            WIRE_FIXED32 => self.take(4).map(drop),
            other => Err(format!("неизвестный тип поля {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кодирует SentencePiece так же, как protobuf.
    fn piece(text: &str, score: f32, kind: u8) -> Vec<u8> {
        let mut inner = vec![0x0A, text.len() as u8];
        inner.extend_from_slice(text.as_bytes());
        inner.push(0x15);
        inner.extend_from_slice(&score.to_le_bytes());
        inner.extend_from_slice(&[0x18, kind]);
        let mut outer = vec![0x0A, inner.len() as u8];
        outer.extend(inner);
        outer
    }

    #[test]
    fn parses_pieces_and_skips_other_fields() {
        let mut model = piece("<unk>", 0.0, 2);
        model.extend(piece("▁д", -3.5, 1));
        // Поле 2 (trainer_spec) — сообщение, должно пропускаться.
        model.extend_from_slice(&[0x12, 0x03, 0x08, 0x96, 0x01]);
        model.extend(piece("жа", -7.25, 1));
        let pieces = parse_model(&model).unwrap();
        assert_eq!(pieces.len(), 3);
        assert_eq!(
            pieces[1],
            Piece {
                text: "▁д".into(),
                score: -3.5
            }
        );
        assert_eq!(vocab_text(&pieces[1..]), "▁д\t-3.5\nжа\t-7.25\n");
    }

    #[test]
    fn rejects_truncated_and_empty_models() {
        let model = piece("▁д", -3.5, 1);
        assert!(parse_model(&model[..model.len() - 2]).is_err());
        assert!(parse_model(&[]).is_err());
    }

    #[test]
    fn real_model_if_present() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../models/sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16/bpe.model",
        );
        let Ok(bytes) = std::fs::read(&path) else {
            return; // моделей нет — проверяется в tests/fixtures.rs
        };
        let pieces = parse_model(&bytes).unwrap();
        // tokens.txt — 502 строки: эти 500 токенов плюс служебные #0 и #1.
        assert_eq!(pieces.len(), 500);
        assert_eq!(pieces[0].text, "<blk>");
        assert!(pieces.iter().any(|p| p.text == "▁д"));
    }
}
