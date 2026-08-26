//! Per-provider adapters and the normalized audio shape.
//!
//! Two kinds of provider:
//!   - **cloud** (`openai`, `elevenlabs`): build the HTTP request, hand it
//!     to `network`'s `http_request` action, parse the response — exactly
//!     the `ai` plugin model.
//!   - **local** (`sherpa`): synthesize in-process via sherpa-onnx; no
//!     HTTP at all. See `crate::provider::sherpa`.

pub mod elevenlabs;
pub mod mp3;
pub mod openai;
pub mod opus;
pub mod sherpa;

use std::collections::HashMap;

use crate::request::{AudioFormat, SynthesizeParams};

/// Mirrors `network`'s `http_request` action params — built by a cloud
/// adapter, serialized as-is into the `ActionRequest.params_json` sent to
/// `network`.
#[derive(Debug, serde::Serialize)]
pub struct HttpRequestJson {
    pub method: &'static str,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub timeout_ms: u64,
}

/// Normalized synthesis result — the shape `tts` returns to its callers
/// in `ActionResponse.data_json`, regardless of provider.
///
/// `sample_rate_hz` / `num_channels` are 0 when they can't be known from
/// the container format alone (e.g. an MP3 body from a cloud provider has
/// no reliable header fields here); WAV and raw-PCM bodies always carry
/// them.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioResult {
    pub format: String,
    pub sample_rate_hz: u32,
    pub num_channels: u8,
    pub duration_seconds: f32,
    pub audio_base64: String,
}

/// One selectable voice, returned by the `tts_voices` action.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VoiceInfo {
    /// Value the caller puts in the `voice` field of `tts_synthesize`.
    pub id: String,
    /// Human-friendly label.
    pub name: String,
}

/// A cloud TTS provider adapter. Local inference is intentionally NOT part
/// of this trait — it doesn't speak HTTP (see `sherpa`).
pub trait Provider {
    /// Build the `network` `http_request` params for one synthesis call.
    /// `api_key` is the resolved secret value (never logged, never echoed
    /// back in any error).
    fn build_http_request(&self, params: &SynthesizeParams, api_key: &str) -> HttpRequestJson;

    /// Parse the provider's raw HTTP response body (called only on 2xx)
    /// into the normalized result. `format` is the caller-requested
    /// `AudioFormat`, which decides how the body is interpreted (MP3
    /// passthrough vs WAV header parse vs known-rate raw PCM).
    fn parse_response(&self, body: &[u8], format: AudioFormat) -> Result<AudioResult, String>;
}

// ---------------------------------------------------------------------------
// WAV encoding / header helpers (used by the sherpa provider and by the
// cloud adapters to recover sample rate + channel count from WAV bodies).
// ---------------------------------------------------------------------------

/// Encode interleaved f32 samples in `[-1, 1]` as a canonical 16-bit PCM
/// WAV file. `samples.len()` must be divisible by `num_channels`.
pub fn f32_to_wav(samples: &[f32], sample_rate: u32, num_channels: u16) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // audio format: PCM
    wav.extend_from_slice(&num_channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * num_channels as u32 * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&(num_channels * 2).to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        wav.extend_from_slice(&v.to_le_bytes());
    }
    wav
}

/// Read `(sample_rate, num_channels)` out of a canonical 16-bit PCM WAV
/// body. Returns `None` when the bytes aren't a WAV we understand.
pub fn wav_info(bytes: &[u8]) -> Option<(u32, u16)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        if chunk_id == b"fmt " {
            if size < 16 || offset + 8 + 16 > bytes.len() {
                return None;
            }
            let fmt = &bytes[offset + 8..];
            if u16::from_le_bytes(fmt[0..2].try_into().ok()?) != 1 {
                return None; // not PCM
            }
            let channels = u16::from_le_bytes(fmt[2..4].try_into().ok()?);
            let rate = u32::from_le_bytes(fmt[4..8].try_into().ok()?);
            return Some((rate, channels));
        }
        offset += 8 + size + (size % 2); // chunks are word-aligned
    }
    None
}

/// Duration in seconds of a canonical 16-bit PCM WAV body, from its `data`
/// chunk size. `None` when the header can't be parsed.
pub fn wav_duration_seconds(bytes: &[u8]) -> Option<f32> {
    let (rate, channels) = wav_info(bytes)?;
    if rate == 0 || channels == 0 {
        return None;
    }
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        if chunk_id == b"data" {
            let data_start = offset + 8;
            let data_len = size.min(bytes.len().saturating_sub(data_start));
            return Some(data_len as f32 / (rate * channels as u32 * 2) as f32);
        }
        offset += 8 + size + (size % 2);
    }
    None
}

/// Base64-encode audio bytes for the `audio_base64` field.
pub fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Kokoro voice table.
//
// The official Kokoro-82M `voices-v1.0.bin` orders its 26 style vectors by
// this fixed list (see the hexgrad/Kokoro-82M model card). sherpa-onnx
// exposes voices only as integer sids, so this table maps the friendly
// names to sids. Callers can always escape with `sid:N` when a custom
// voices file uses a different order.
// ---------------------------------------------------------------------------

pub const KOKORO_VOICES: [(&str, i32); 26] = [
    ("af_heart", 0),
    ("af_bella", 1),
    ("af_nicole", 2),
    ("af_aoede", 3),
    ("af_kore", 4),
    ("af_sarah", 5),
    ("af_nova", 6),
    ("af_sky", 7),
    ("am_adam", 8),
    ("am_echo", 9),
    ("am_eric", 10),
    ("am_fenrir", 11),
    ("am_liam", 12),
    ("am_michael", 13),
    ("am_onyx", 14),
    ("am_puck", 15),
    ("am_santa", 16),
    ("bf_alice", 17),
    ("bf_emma", 18),
    ("bf_isabella", 19),
    ("bf_lily", 20),
    ("bm_daniel", 21),
    ("bm_fable", 22),
    ("bm_george", 23),
    ("bm_lewis", 24),
    ("ff_siwis", 25),
];

/// Look up a Kokoro voice name's sid, or parse `sid:N` for a raw index.
pub fn kokoro_voice_to_sid(voice: &str) -> Result<i32, String> {
    if let Some(idx) = voice.strip_prefix("sid:") {
        return idx
            .parse::<i32>()
            .map_err(|_| format!("invalid sid: {voice}"));
    }
    KOKORO_VOICES
        .iter()
        .find(|(name, _)| *name == voice)
        .map(|(_, sid)| *sid)
        .ok_or_else(|| {
            let names: Vec<&str> = KOKORO_VOICES.iter().map(|(n, _)| *n).collect();
            format!(
                "unknown kokoro voice '{voice}' (known: {})",
                names.join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_wav_writes_valid_header() {
        let samples = [0.0f32, 0.5, -0.5, 1.0, -1.0];
        let wav = f32_to_wav(&samples, 24000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + samples.len() * 2);
    }

    #[test]
    fn wav_info_roundtrips_our_encoder() {
        let wav = f32_to_wav(&[0.0; 100], 16000, 2);
        assert_eq!(wav_info(&wav), Some((16000, 2)));
    }

    #[test]
    fn wav_duration_from_data_chunk() {
        // 16000 Hz, mono, 16-bit: 32000 bytes/sec; 64000 bytes -> 2.0 s
        let wav = f32_to_wav(&[0.0; 32000], 16000, 1);
        assert_eq!(wav_duration_seconds(&wav), Some(2.0));
    }

    #[test]
    fn wav_duration_none_for_garbage() {
        assert_eq!(wav_duration_seconds(b"nope"), None);
    }

    #[test]
    fn wav_info_rejects_non_wav() {
        assert_eq!(wav_info(b"not a wav at all"), None);
    }

    #[test]
    fn wav_info_rejects_non_pcm() {
        let mut wav = f32_to_wav(&[0.0; 10], 8000, 1);
        wav[20..22].copy_from_slice(&3u16.to_le_bytes()); // float format
        assert_eq!(wav_info(&wav), None);
    }

    #[test]
    fn kokoro_voice_lookup_known_name() {
        assert_eq!(kokoro_voice_to_sid("af_heart").unwrap(), 0);
        assert_eq!(kokoro_voice_to_sid("ff_siwis").unwrap(), 25);
    }

    #[test]
    fn kokoro_voice_rejects_unknown_name() {
        let err = kokoro_voice_to_sid("zz_unknown").unwrap_err();
        assert!(err.contains("unknown kokoro voice"), "error was: {err}");
    }

    #[test]
    fn kokoro_voice_parses_sid_escape() {
        assert_eq!(kokoro_voice_to_sid("sid:7").unwrap(), 7);
        assert!(kokoro_voice_to_sid("sid:abc").is_err());
    }
}
