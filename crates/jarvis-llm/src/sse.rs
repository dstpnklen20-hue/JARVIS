//! Разбор потока Server-Sent Events: строки `event:` / `data:`, пустая
//! строка завершает событие. Так приходит потоковый ответ Messages API.

use std::io::BufRead;

/// Одно событие потока.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Читает события по одному. `Ok(None)` — поток закончился.
pub struct SseReader<R> {
    reader: R,
    line: String,
}

impl<R: BufRead> SseReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line: String::new(),
        }
    }

    pub fn next_event(&mut self) -> std::io::Result<Option<SseEvent>> {
        let mut event = String::new();
        let mut data = String::new();
        let mut seen_any = false;
        loop {
            self.line.clear();
            let n = self.reader.read_line(&mut self.line)?;
            if n == 0 {
                // Конец потока; незавершённое событие тоже отдаём.
                return Ok(seen_any.then_some(SseEvent { event, data }));
            }
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if seen_any {
                    return Ok(Some(SseEvent { event, data }));
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // комментарий
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "event" => {
                    event = value.to_owned();
                    seen_any = true;
                }
                "data" => {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                    seen_any = true;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_events_separated_by_blank_lines() {
        let raw = "event: message_start\ndata: {\"a\":1}\n\n: comment\nevent: ping\ndata: {}\n\r\nevent: x\ndata: line1\ndata: line2\n";
        let mut r = SseReader::new(raw.as_bytes());
        assert_eq!(
            r.next_event().unwrap(),
            Some(SseEvent {
                event: "message_start".into(),
                data: "{\"a\":1}".into()
            })
        );
        assert_eq!(r.next_event().unwrap().unwrap().event, "ping");
        let last = r.next_event().unwrap().unwrap();
        assert_eq!(last.data, "line1\nline2");
        assert_eq!(r.next_event().unwrap(), None);
    }
}
