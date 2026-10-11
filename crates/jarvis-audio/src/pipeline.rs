//! Конвейер слуха в отдельном потоке:
//! микрофон → ресемплинг до 16 кГц → слово активации → проверка голоса → событие;
//! после «Джарвис» — распознавание команды (с начала слова активации, из
//! буфера, — так команда «на одном дыхании» тоже попадает в текст).
//!
//! - На паузе микрофон закрывается (индикатор микрофона Windows гаснет).
//! - Пока Джарвис говорит, вход заглушается ([`MuteGate`]) — он не слышит сам себя.
//! - Пропал микрофон — конвейер сообщает об этом и пытается открыть его снова.
//! - Паника внутри конвейера не роняет приложение: поток перезапускается.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::asr::Hotwords;
use crate::capture::Capture;
use crate::engines::{self, Engines};
use crate::models::SpeechModels;
use crate::resample::{MODEL_RATE, StreamResampler};
use crate::speaker::{SpeakerEncoder, Voiceprint};
use crate::stt::{SherpaStt, SpeechToText, Transcript};
use crate::wake::{SherpaWakeWord, WakeMatcher, WakeWordEngine};

/// Сколько звука до начала слова активации отдать распознавателю команд.
const PREROLL_MARGIN_SECS: f32 = 0.3;

/// Сколько последних секунд звука держать для проверки голоса.
const HISTORY_SECS: usize = 3;
/// Пауза перед повторной попыткой открыть микрофон.
const REOPEN_DELAY: Duration = Duration::from_secs(2);
/// Перезапусков после паники, после которых конвейер сдаётся.
const MAX_RESTARTS: u32 = 5;
/// Сколько ждать новых отсчётов, если буфер пуст.
const IDLE_WAIT: Duration = Duration::from_millis(10);

pub struct PipelineSettings {
    /// `None` — системный микрофон.
    pub device: Option<String>,
    pub models: SpeechModels,
    pub matcher: WakeMatcher,
    /// Образец голоса и порог; `None` — голос не проверяется.
    pub voiceprint: Option<(Voiceprint, f32)>,
    /// Тишина в конце команды, секунды.
    pub end_silence: f32,
    /// Подсказки распознавателю команд (фразы команд).
    pub hotwords: Option<Hotwords>,
}

#[derive(Debug, Clone)]
pub struct WakeEvent {
    pub heard: String,
    pub detected_at: Instant,
    /// Сходство с образцом голоса, если проверка включена.
    pub similarity: Option<f32>,
    pub verify_time: Duration,
}

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// Модели загружены и прогреты.
    Ready {
        load_time: Duration,
    },
    /// Микрофон открыт.
    Listening {
        device: String,
        sample_rate: u32,
    },
    Wake(WakeEvent),
    /// Текст команды (частичный или итоговый).
    Transcript(Transcript),
    /// Слово услышано, но голос не совпал с образцом.
    Rejected {
        heard: String,
        similarity: f32,
        threshold: f32,
    },
    /// Микрофон недоступен; конвейер будет пробовать снова.
    CaptureLost(String),
    /// Ошибка, после которой конвейер не работает (не загрузились модели и т. п.).
    Fatal(String),
}

enum Control {
    Listen(bool),
    Transcribe(bool),
    Hotwords(Vec<String>),
    Shutdown,
}

/// Заглушка входа на время собственной речи.
#[derive(Debug)]
pub struct MuteGate {
    epoch: Instant,
    until_ms: AtomicU64,
}

impl Default for MuteGate {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            until_ms: AtomicU64::new(0),
        }
    }
}

impl MuteGate {
    pub fn mute_for(&self, duration: Duration) {
        let until = (self.epoch.elapsed() + duration).as_millis() as u64;
        self.until_ms.fetch_max(until, Ordering::Relaxed);
    }

    pub fn is_muted(&self) -> bool {
        (self.epoch.elapsed().as_millis() as u64) < self.until_ms.load(Ordering::Relaxed)
    }
}

pub type EventSink = Box<dyn Fn(PipelineEvent) + Send>;

pub struct Pipeline {
    control: Sender<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Pipeline {
    pub fn spawn(
        settings: PipelineSettings,
        events: EventSink,
        gate: Arc<MuteGate>,
    ) -> std::io::Result<Self> {
        let (control, control_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("jarvis-listen".into())
            .spawn(move || supervise(settings, events, gate, control_rx))?;
        Ok(Self {
            control,
            thread: Some(thread),
        })
    }

    /// Включить/выключить прослушивание (выключенное освобождает микрофон).
    pub fn set_listening(&self, on: bool) {
        let _ = self.control.send(Control::Listen(on));
    }

    /// Включить/выключить распознавание команд. Слово активации включает
    /// его само; выключает ядро, когда команды больше не ждёт.
    pub fn set_transcribing(&self, on: bool) {
        let _ = self.control.send(Control::Transcribe(on));
    }

    /// Новые подсказки распознавателю команд (после перезагрузки команд).
    pub fn set_hotwords(&self, phrases: Vec<String>) {
        let _ = self.control.send(Control::Hotwords(phrases));
    }

    /// Остановить поток и дождаться его завершения.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.control.send(Control::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("поток слуха завершился паникой");
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

fn supervise(
    settings: PipelineSettings,
    events: EventSink,
    gate: Arc<MuteGate>,
    control: Receiver<Control>,
) {
    let mut restarts = 0;
    loop {
        let run = catch_unwind(AssertUnwindSafe(|| {
            worker(&settings, &events, &gate, &control)
        }));
        match run {
            Ok(()) => return,
            Err(panic) => {
                let message = panic_message(&panic);
                restarts += 1;
                tracing::error!(%message, restarts, "сбой в потоке слуха, перезапуск");
                if restarts > MAX_RESTARTS {
                    events(PipelineEvent::Fatal(format!(
                        "поток слуха падал {restarts} раз подряд: {message}"
                    )));
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "неизвестная паника".into())
}

/// Состояние одного запуска конвейера.
struct Worker<'a> {
    settings: &'a PipelineSettings,
    events: &'a EventSink,
    gate: &'a MuteGate,
    wake: SherpaWakeWord,
    stt: SherpaStt,
    /// Распознаются ли сейчас команды.
    transcribing: bool,
    speaker: Option<SpeakerEncoder>,
    history: VecDeque<f32>,
    scratch: Vec<f32>,
    resampled: Vec<f32>,
}

fn worker(
    settings: &PipelineSettings,
    events: &EventSink,
    gate: &MuteGate,
    control: &Receiver<Control>,
) {
    let started = Instant::now();
    // Один распознаватель на слово активации и команды — одна модель в памяти.
    let Engines { wake, stt } = match engines::build(
        &settings.models.asr,
        settings.matcher.clone(),
        settings.end_silence,
        settings.hotwords.as_ref(),
    ) {
        Ok(e) => e,
        Err(e) => {
            events(PipelineEvent::Fatal(jarvis_core::error_chain(&e)));
            return;
        }
    };
    let speaker = match &settings.voiceprint {
        None => None,
        Some(_) => match SpeakerEncoder::new(&settings.models.speaker) {
            Ok(encoder) => {
                // Прогрев: первый расчёт заметно медленнее.
                let _ = encoder.embed(&vec![0.0; MODEL_RATE as usize]);
                Some(encoder)
            }
            Err(e) => {
                events(PipelineEvent::Fatal(jarvis_core::error_chain(&e)));
                return;
            }
        },
    };
    events(PipelineEvent::Ready {
        load_time: started.elapsed(),
    });

    let mut w = Worker {
        settings,
        events,
        gate,
        wake,
        stt,
        transcribing: false,
        speaker,
        history: VecDeque::with_capacity(MODEL_RATE as usize * HISTORY_SECS),
        scratch: Vec::new(),
        resampled: Vec::new(),
    };
    w.run(control);
}

impl Worker<'_> {
    fn run(&mut self, control: &Receiver<Control>) {
        let mut listening = true;
        let mut capture: Option<(Capture, StreamResampler)> = None;
        let mut lost_reported = false;
        let mut dropped_seen = 0u64;

        loop {
            // Команды. На паузе — блокирующее ожидание, микрофон закрыт.
            let timeout = match (&capture, listening) {
                (_, false) => None,
                (None, true) => Some(REOPEN_DELAY),
                (Some(_), true) => Some(Duration::ZERO),
            };
            let command = match timeout {
                None => control.recv().ok().or(Some(Control::Shutdown)),
                Some(t) if t.is_zero() => control.try_recv().ok(),
                Some(t) => match control.recv_timeout(t) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => Some(Control::Shutdown),
                },
            };
            match command {
                Some(Control::Shutdown) => return,
                Some(Control::Listen(on)) => {
                    listening = on;
                    if !on {
                        capture = None;
                        self.wake.reset();
                        self.transcribing = false;
                        self.history.clear();
                        tracing::info!("прослушивание на паузе, микрофон освобождён");
                    }
                    continue;
                }
                Some(Control::Transcribe(on)) => {
                    if on && !self.transcribing {
                        self.stt.reset();
                    }
                    self.transcribing = on;
                    continue;
                }
                Some(Control::Hotwords(phrases)) => {
                    if self.settings.hotwords.is_some() {
                        self.stt.set_hotwords(&phrases);
                    }
                    continue;
                }
                None => {}
            }
            if !listening {
                continue;
            }

            // Микрофон.
            if capture.is_none() {
                match self.open_capture() {
                    Ok(opened) => {
                        lost_reported = false;
                        dropped_seen = 0;
                        capture = Some(opened);
                    }
                    Err(e) => {
                        if !lost_reported {
                            (self.events)(PipelineEvent::CaptureLost(jarvis_core::error_chain(&e)));
                            lost_reported = true;
                        }
                        continue;
                    }
                }
            }
            let Some((cap, resampler)) = capture.as_mut() else {
                continue;
            };
            if cap.stats().failed.load(Ordering::Acquire) {
                (self.events)(PipelineEvent::CaptureLost(format!(
                    "микрофон «{}» перестал отвечать",
                    cap.device_name()
                )));
                lost_reported = true;
                capture = None;
                self.wake.reset();
                continue;
            }
            let dropped = cap.stats().dropped.load(Ordering::Relaxed);
            if dropped > dropped_seen {
                tracing::warn!(
                    samples = dropped - dropped_seen,
                    "обработка не успевала, часть звука потеряна"
                );
                dropped_seen = dropped;
            }

            // Звук.
            let chunk = (cap.sample_rate() as usize / 50).max(256); // ~20 мс
            self.scratch.resize(chunk, 0.0);
            let n = cap.read(&mut self.scratch);
            if n == 0 {
                std::thread::sleep(IDLE_WAIT);
                continue;
            }
            self.resampled.clear();
            let resampled = &mut self.resampled;
            if let Err(e) =
                resampler.process(&self.scratch[..n], |out| resampled.extend_from_slice(out))
            {
                tracing::error!(error = %e, "ошибка ресемплинга, переоткрываю микрофон");
                capture = None;
                continue;
            }
            self.process_16k();
        }
    }

    fn open_capture(&self) -> Result<(Capture, StreamResampler), crate::AudioError> {
        let capture = Capture::open(self.settings.device.as_deref())?;
        let resampler = StreamResampler::new(capture.sample_rate(), MODEL_RATE)?;
        (self.events)(PipelineEvent::Listening {
            device: capture.device_name().to_owned(),
            sample_rate: capture.sample_rate(),
        });
        Ok((capture, resampler))
    }

    fn process_16k(&mut self) {
        if self.resampled.is_empty() {
            return;
        }
        // Пока Джарвис говорит — подаём тишину: время потока идёт, а своего
        // голоса модель не слышит.
        if self.gate.is_muted() {
            self.resampled.iter_mut().for_each(|s| *s = 0.0);
        }
        let cap = MODEL_RATE as usize * HISTORY_SECS;
        self.history.extend(self.resampled.iter().copied());
        if self.history.len() > cap {
            let excess = self.history.len() - cap;
            self.history.drain(..excess);
        }

        if self.transcribing
            && let Some(transcript) = self.stt.accept(&self.resampled)
        {
            (self.events)(PipelineEvent::Transcript(transcript));
        }

        let Some(detection) = self.wake.accept(&self.resampled) else {
            return;
        };
        let detected_at = Instant::now();

        let (similarity, verify_time) = match (&self.settings.voiceprint, &self.speaker) {
            (Some((print, threshold)), Some(encoder)) => {
                let started = Instant::now();
                let secs = detection.started_secs_ago.unwrap_or(1.0) + 0.15;
                let len = ((secs.clamp(0.5, 2.5)) * MODEL_RATE as f32) as usize;
                let len = len.min(self.history.len());
                let segment: Vec<f32> = self
                    .history
                    .iter()
                    .skip(self.history.len() - len)
                    .copied()
                    .collect();
                let similarity = encoder.embed(&segment).map(|e| print.similarity(&e));
                let elapsed = started.elapsed();
                match similarity {
                    Some(s) if s >= *threshold => (Some(s), elapsed),
                    other => {
                        (self.events)(PipelineEvent::Rejected {
                            heard: detection.heard,
                            similarity: other.unwrap_or(0.0),
                            threshold: *threshold,
                        });
                        return;
                    }
                }
            }
            _ => (None, Duration::ZERO),
        };

        (self.events)(PipelineEvent::Wake(WakeEvent {
            heard: detection.heard,
            detected_at,
            similarity,
            verify_time,
        }));

        // Сразу включаем распознавание команды и отдаём ему звук с начала
        // слова активации: «Джарвис, открой ютуб» на одном дыхании.
        if !self.transcribing {
            self.transcribing = true;
            self.stt.reset();
            let secs = detection.started_secs_ago.unwrap_or(1.0) + PREROLL_MARGIN_SECS;
            let len = ((secs * MODEL_RATE as f32) as usize).min(self.history.len());
            let preroll: Vec<f32> = self
                .history
                .iter()
                .skip(self.history.len() - len)
                .copied()
                .collect();
            if let Some(transcript) = self.stt.accept(&preroll) {
                (self.events)(PipelineEvent::Transcript(transcript));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_gate_mutes_for_duration() {
        let gate = MuteGate::default();
        assert!(!gate.is_muted());
        gate.mute_for(Duration::from_millis(80));
        assert!(gate.is_muted());
        std::thread::sleep(Duration::from_millis(120));
        assert!(!gate.is_muted());
    }

    #[test]
    fn mute_gate_never_shortens() {
        let gate = MuteGate::default();
        gate.mute_for(Duration::from_secs(10));
        gate.mute_for(Duration::from_millis(1));
        assert!(gate.is_muted());
    }

    #[test]
    fn panic_message_extracts_text() {
        let p: Box<dyn std::any::Any + Send> = Box::new("сбой");
        assert_eq!(panic_message(&p), "сбой");
        let p: Box<dyn std::any::Any + Send> = Box::new(String::from("сбой 2"));
        assert_eq!(panic_message(&p), "сбой 2");
    }
}
