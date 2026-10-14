//! `jarvis --say "текст"`: произнести фразу голосом синтеза (или сохранить
//! в WAV с `--out`) — послушать голоса и темп, не запуская Джарвиса.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_audio::pack::VoicePack;
use jarvis_audio::player::Player;
use jarvis_audio::tts::{PiperTts, SynthParams, TextToSpeech};
use jarvis_audio::wav;
use jarvis_core::Config;
use jarvis_core::config::resolve;

pub fn run(
    root: &Path,
    config: &Config,
    text: &str,
    voice: Option<&str>,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    let name = match voice {
        Some(name) => name,
        None => config
            .voice
            .tts_voice()
            .context("синтез выключен (voice.tts = \"none\"); укажите голос: --voice dmitri")?,
    };
    let model = jarvis_audio::models::tts_voice(root, config, name).with_context(|| {
        let installed = jarvis_audio::models::installed_voices(root, config);
        format!(
            "голос «{name}» не установлен (установлены: {}); остальные: scripts\\download-models.ps1 -Full",
            if installed.is_empty() {
                "нет".into()
            } else {
                installed.join(", ")
            }
        )
    })?;

    let started = Instant::now();
    let tts = PiperTts::new(&model, SynthParams::from_config(&config.voice))?;
    let load = started.elapsed();
    let started = Instant::now();
    let clip = tts.synthesize(text)?.trimmed();
    let synth = started.elapsed();
    println!(
        "Голос {name}: загрузка {} мс, синтез {} мс, {:.1} с речи (RTF {:.3})",
        load.as_millis(),
        synth.as_millis(),
        clip.duration_secs(),
        synth.as_secs_f32() / clip.duration_secs().max(0.01)
    );

    if let Some(path) = out {
        wav::write(path, &clip)?;
        println!("Сохранено: {}", path.display());
        return Ok(());
    }
    // Плеер открывается со звуковым пакетом — он же играет ответы демона.
    let pack = VoicePack::load(&resolve(root, &config.paths.sounds).join(&config.voice.pack))
        .context("звуковой пакет не загружен")?;
    let mut player = Player::new(&pack, config.voice.volume).context("вывод звука не открылся")?;
    let duration = player.play_clip(&clip)?;
    std::thread::sleep(duration + Duration::from_millis(200));
    Ok(())
}
