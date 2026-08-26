//! Local STT provider: sherpa-onnx, in-process, no HTTP.
//!
//! Loads an offline ASR model from disk (lazily on the first transcribe
//! request, cached for the process lifetime) and runs inference via the
//! `sherpa-onnx` crate. Two model families, selected by the operator with
//! `STT_PLUGIN_LOCAL_MODEL_TYPE`:
//!
//!   - `transducer` — zipformer offline models. Files in
//!     `STT_PLUGIN_LOCAL_MODEL_DIR`: `encoder.onnx`, `decoder.onnx`,
//!     `joiner.onnx`, `tokens.txt` (e.g. a `sherpa-onnx-zipformer-*`
//!     model pack).
//!   - `whisper` — Whisper converted to ONNX. Files: `encoder.onnx`,
//!     `decoder.onnx`, `tokens.txt` (e.g. `sherpa-onnx-whisper-*`).
//!     `STT_PLUGIN_LOCAL_LANGUAGE` (default `"en"`) selects the decoding
//!     language; a caller-supplied `language` param overrides it per
//!     request via a stream option.
//!
//! Both families expect 16 kHz audio; the recognizer resamples whatever
//! the caller uploads to its feature rate.

use std::path::Path;
use std::sync::OnceLock;

use crate::provider::{ModelInfo, TranscriptResult};
use crate::request::{AudioFormat, TranscribeParams};

pub const ENV_MODEL_DIR: &str = "STT_PLUGIN_LOCAL_MODEL_DIR";
pub const ENV_MODEL_TYPE: &str = "STT_PLUGIN_LOCAL_MODEL_TYPE";
pub const ENV_NUM_THREADS: &str = "STT_PLUGIN_LOCAL_NUM_THREADS";
pub const ENV_LANGUAGE: &str = "STT_PLUGIN_LOCAL_LANGUAGE";

const DEFAULT_NUM_THREADS: i32 = 2;
const DEFAULT_LANGUAGE: &str = "en";

/// Decodes 16-bit PCM samples (little-endian, any channel count) from the
/// given input format. WAV headers are parsed (not just skipped); raw `pcm`
/// requires the caller-supplied `sample_rate_hz` / `num_channels`.
pub fn decode_samples(
    audio: &[u8],
    format: AudioFormat,
    sample_rate_hz: u32,
    num_channels: u16,
) -> Result<Vec<i16>, String> {
    let (pcm, rate, channels) = match format {
        AudioFormat::Wav => {
            let (pcm, rate, channels) = parse_wav(audio)?;
            (pcm, rate, channels)
        }
        AudioFormat::Pcm => {
            if num_channels == 0 {
                return Err("pcm input requires num_channels".to_string());
            }
            if sample_rate_hz == 0 {
                return Err("pcm input requires sample_rate_hz".to_string());
            }
            if !audio.len().is_multiple_of(2) {
                return Err("pcm audio byte length must be even (16-bit samples)".to_string());
            }
            let samples: Vec<i16> = audio
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            (samples, sample_rate_hz, num_channels)
        }
        _ => return Err("sherpa accepts wav|pcm input only".to_string()),
    };

    let mono = downmix(&pcm, channels as usize);
    let _ = rate; // rate is validated against the model by the caller
    Ok(mono)
}

/// Downmix interleaved multi-channel samples to mono by averaging, and
/// keep mono as-is.
pub fn downmix(samples: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let frames = samples.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in samples.chunks_exact(channels) {
        let sum: i32 = frame.iter().map(|&s| s as i32).sum();
        mono.push((sum / channels as i32) as i16);
    }
    mono
}

/// A 16-bit PCM WAV: `RIFF`/`WAVE`, a `fmt ` chunk, and a `data` chunk.
/// Parsed strictly so malformed headers fail loudly instead of producing
/// garbage audio.
pub fn parse_wav(bytes: &[u8]) -> Result<(Vec<i16>, u32, u16), String> {
    if bytes.len() < 44 {
        return Err("wav file too short (< 44 bytes)".to_string());
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }
    if &bytes[12..16] != b"fmt " {
        return Err("wav missing fmt chunk".to_string());
    }
    let fmt_size = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    if fmt_size < 16 {
        return Err("wav fmt chunk too small".to_string());
    }
    let audio_format = u16::from_le_bytes(bytes[20..22].try_into().unwrap());
    if audio_format != 1 {
        return Err(format!(
            "unsupported wav encoding: {audio_format} (expected 1 = PCM)"
        ));
    }
    let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let bits_per_sample = u16::from_le_bytes(bytes[34..36].try_into().unwrap());
    if bits_per_sample != 16 {
        return Err(format!(
            "unsupported wav bit depth: {bits_per_sample} (expected 16)"
        ));
    }
    if channels == 0 {
        return Err("wav has 0 channels".to_string());
    }

    // Find the data chunk; skip any chunks between fmt and data.
    let mut offset = 20 + fmt_size + (fmt_size % 2);
    let mut data_len = 0usize;
    let mut data_start = 0usize;
    while offset + 8 <= bytes.len() {
        if &bytes[offset..offset + 4] == b"data" {
            data_len =
                u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            data_start = offset + 8;
            break;
        }
        let chunk_len =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8 + chunk_len + (chunk_len % 2);
    }
    if data_start == 0 {
        return Err("wav missing data chunk".to_string());
    }
    let available = bytes.len().saturating_sub(data_start).min(data_len);
    if !available.is_multiple_of(2) {
        return Err("wav data chunk has odd byte length (expected 16-bit samples)".to_string());
    }
    let samples: Vec<i16> = bytes[data_start..data_start + available]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok((samples, sample_rate, channels))
}

/// The loaded engine plus the model family it was built for.
struct LoadedEngine {
    recognizer: sherpa_onnx::OfflineRecognizer,
    model_type: String,
    language: String,
}

static ENGINE: OnceLock<Result<LoadedEngine, String>> = OnceLock::new();

/// Lazy, process-lifetime model load. A failed load is cached too — a bad
/// operator config won't be retried (and error-flooded) on every request;
/// it fails fast and consistently until the process restarts.
fn engine() -> Result<&'static LoadedEngine, String> {
    match ENGINE.get_or_init(load_engine) {
        Ok(loaded) => Ok(loaded),
        Err(e) => Err(e.clone()),
    }
}

fn load_engine() -> Result<LoadedEngine, String> {
    let model_dir = std::env::var(ENV_MODEL_DIR)
        .map_err(|_| format!("{ENV_MODEL_DIR} is not set (local provider needs a model dir)"))?;
    let dir = Path::new(&model_dir);
    if !dir.is_dir() {
        return Err(format!("{ENV_MODEL_DIR}={model_dir} is not a directory"));
    }

    let model_type = std::env::var(ENV_MODEL_TYPE).map_err(|_| {
        format!("{ENV_MODEL_TYPE} is not set (local provider needs 'transducer' or 'whisper')")
    })?;
    let model_type = model_type_str(&model_type)?;

    let num_threads = match std::env::var(ENV_NUM_THREADS) {
        Ok(raw) => raw
            .parse::<i32>()
            .map_err(|_| format!("{ENV_NUM_THREADS}={raw} is not an integer"))?,
        Err(_) => DEFAULT_NUM_THREADS,
    };

    let language = match std::env::var(ENV_LANGUAGE) {
        Ok(raw) if !raw.trim().is_empty() => raw,
        _ => DEFAULT_LANGUAGE.to_string(),
    };

    let config = match model_type {
        "transducer" => build_transducer_config(dir, num_threads)?,
        "whisper" => build_whisper_config(dir, num_threads, &language)?,
        _ => unreachable!("model_type_str validated"),
    };

    let recognizer = sherpa_onnx::OfflineRecognizer::create(&config).ok_or_else(|| {
        format!(
            "sherpa-onnx failed to load model from {ENV_MODEL_DIR}={model_dir} \
             (check the file layout for model type '{model_type}', see README.md)"
        )
    })?;

    Ok(LoadedEngine {
        recognizer,
        model_type: model_type.to_string(),
        language,
    })
}

fn model_type_str(raw: &str) -> Result<&'static str, String> {
    match raw {
        "transducer" => Ok("transducer"),
        "whisper" => Ok("whisper"),
        other => Err(format!(
            "{ENV_MODEL_TYPE}={other} is unsupported (use 'transducer' or 'whisper')"
        )),
    }
}

/// Files relative to the model dir that every family needs.
fn require_file(dir: &Path, name: &str) -> Result<String, String> {
    let path = dir.join(name);
    if !path.is_file() {
        return Err(format!(
            "missing required model file: {} (expected at {ENV_MODEL_DIR}/{name})",
            path.display()
        ));
    }
    Ok(path.to_string_lossy().into_owned())
}

fn build_transducer_config(
    dir: &Path,
    num_threads: i32,
) -> Result<sherpa_onnx::OfflineRecognizerConfig, String> {
    let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
    config.model_config.transducer = sherpa_onnx::OfflineTransducerModelConfig {
        encoder: Some(require_file(dir, "encoder.onnx")?),
        decoder: Some(require_file(dir, "decoder.onnx")?),
        joiner: Some(require_file(dir, "joiner.onnx")?),
    };
    config.model_config.tokens = Some(require_file(dir, "tokens.txt")?);
    config.model_config.num_threads = num_threads;
    Ok(config)
}

fn build_whisper_config(
    dir: &Path,
    num_threads: i32,
    language: &str,
) -> Result<sherpa_onnx::OfflineRecognizerConfig, String> {
    let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
    config.model_config.whisper = sherpa_onnx::OfflineWhisperModelConfig {
        encoder: Some(require_file(dir, "encoder.onnx")?),
        decoder: Some(require_file(dir, "decoder.onnx")?),
        language: Some(language.to_string()),
        task: Some("transcribe".to_string()),
        tail_paddings: 0,
        enable_token_timestamps: false,
        enable_segment_timestamps: false,
    };
    config.model_config.tokens = Some(require_file(dir, "tokens.txt")?);
    config.model_config.num_threads = num_threads;
    Ok(config)
}

/// Transcribe `params.audio` with the local engine. CPU-bound and blocking;
/// the serve loop runs one request at a time, so this is fine — same model
/// as `tts`'s local synthesis.
pub fn transcribe(params: &TranscribeParams) -> Result<TranscriptResult, String> {
    let samples = decode_samples(
        &params.audio,
        params.format,
        params.sample_rate_hz,
        params.num_channels,
    )?;
    let rate = match params.format {
        AudioFormat::Wav => parse_wav(&params.audio).map(|(_, r, _)| r)?,
        AudioFormat::Pcm => params.sample_rate_hz,
        _ => return Err("sherpa accepts wav|pcm input only".to_string()),
    };
    transcribe_pcm(&samples, rate, params.language.as_deref())
}

/// Transcribe ready-to-go mono `i16` samples (already decoded/downmixed).
/// Shared by `stt_transcribe` and the streaming listen path, which
/// accumulates `AudioStreamChunk` PCM into a buffer before transcribing.
pub fn transcribe_pcm(
    samples: &[i16],
    rate: u32,
    language: Option<&str>,
) -> Result<TranscriptResult, String> {
    let loaded = engine()?;

    let stream = loaded.recognizer.create_stream();
    if let Some(lang) = language {
        stream.set_option("language", lang);
    }
    let floats: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
    stream.accept_waveform(rate as i32, &floats);
    loaded.recognizer.decode(&stream);

    let text = stream
        .get_result()
        .map(|r| r.text.trim().to_string())
        .unwrap_or_default();

    Ok(TranscriptResult {
        text,
        language: language
            .map(str::to_string)
            .unwrap_or_else(|| loaded.language.clone()),
        duration_seconds: samples.len() as f32 / rate.max(1) as f32,
        model: format!("sherpa:{}", loaded.model_type),
    })
}

/// The model the local engine exposes. There's exactly one (the operator's
/// configured model) — no selectable alternatives like TTS voices.
pub fn models() -> Result<Vec<ModelInfo>, String> {
    let loaded = engine()?;
    Ok(vec![ModelInfo {
        id: format!("sherpa:{}", loaded.model_type),
        name: format!("local sherpa-onnx model ({})", loaded.model_type),
    }])
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Build a canonical 16-bit PCM wav with `data_len` zero bytes of audio.
    /// Used by this crate's other test modules too.
    pub fn fixture_wav(sample_rate: u32, channels: u16, data_len: usize) -> Vec<u8> {
        assert!(data_len.is_multiple_of(2), "fixture data_len must be even");
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // audio format: PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes()); // byte rate
        wav.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        wav.extend_from_slice(&vec![0u8; data_len]);
        wav
    }

    #[test]
    fn model_type_validates() {
        assert_eq!(model_type_str("transducer").unwrap(), "transducer");
        assert_eq!(model_type_str("whisper").unwrap(), "whisper");
        assert!(model_type_str("wavenet").is_err());
    }

    #[test]
    fn require_file_reports_missing() {
        let err = require_file(Path::new("/nonexistent-dir"), "encoder.onnx").unwrap_err();
        assert!(
            err.contains("missing required model file"),
            "error was: {err}"
        );
    }

    #[test]
    fn wav_parse_roundtrip() {
        let samples: Vec<i16> = (0..10).map(|i| i as i16).collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + samples.len() as u32 * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        for s in &samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }

        let (parsed, rate, channels) = parse_wav(&wav).unwrap();
        assert_eq!(parsed, samples);
        assert_eq!(rate, 16000);
        assert_eq!(channels, 1);
    }

    #[test]
    fn wav_parse_skips_extra_chunks() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&100u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        // A LIST chunk with 4 bytes of junk, then data.
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[1u8, 2, 3, 4]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&2u32.to_le_bytes());
        wav.extend_from_slice(&7i16.to_le_bytes());

        let (parsed, _, _) = parse_wav(&wav).unwrap();
        assert_eq!(parsed, vec![7i16]);
    }

    #[test]
    fn wav_parse_rejects_garbage() {
        assert!(parse_wav(b"not a wave at all").is_err());
        assert!(parse_wav(&[0u8; 44]).is_err()); // RIFF header wrong
    }

    #[test]
    fn wav_parse_rejects_float_encoding() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&64000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[0u8; 4]);
        let err = parse_wav(&wav).unwrap_err();
        assert!(err.contains("encoding"), "error was: {err}");
    }

    #[test]
    fn downmix_stereo_averages() {
        let stereo = [10i16, 20, 30, 40, 50, 60];
        let mono = downmix(&stereo, 2);
        assert_eq!(mono, vec![15, 35, 55]);
    }

    #[test]
    fn downmix_mono_passthrough() {
        let mono = [10i16, 20, 30];
        assert_eq!(downmix(&mono, 1), mono);
    }

    #[test]
    fn decode_rejects_mp3_for_sherpa() {
        let err = decode_samples(&[0u8; 64], AudioFormat::Mp3, 16000, 1).unwrap_err();
        assert!(err.contains("wav|pcm"), "error was: {err}");
    }

    #[test]
    fn decode_pcm_requires_metadata() {
        let err = decode_samples(&[0u8; 4], AudioFormat::Pcm, 0, 1).unwrap_err();
        assert!(err.contains("sample_rate_hz"), "error was: {err}");

        let err = decode_samples(&[0u8; 4], AudioFormat::Pcm, 16000, 0).unwrap_err();
        assert!(err.contains("num_channels"), "error was: {err}");
    }

    #[test]
    fn decode_pcm_rejects_odd_length() {
        let err = decode_samples(&[0u8; 5], AudioFormat::Pcm, 16000, 1).unwrap_err();
        assert!(err.contains("even"), "error was: {err}");
    }

    #[test]
    fn decode_wav_downsamples_stereo() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&64000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8u32.to_le_bytes());
        for s in [10i16, 20, 30, 40] {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        let mono = decode_samples(&wav, AudioFormat::Wav, 0, 0).unwrap();
        assert_eq!(mono, vec![15, 35]);
    }
}
