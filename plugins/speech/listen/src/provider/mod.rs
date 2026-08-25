//! Per-provider adapters and the normalized transcript shape.
//!
//! Two kinds of provider:
//!   - **cloud** (`openai`): build the HTTP request (a `multipart/form-data`
//!     audio upload), hand it to `network`'s `http_request` action, parse
//!     the response — exactly the `ai`/`tts` plugin model. The multipart
//!     body is base64-encoded into `body_base64` because `network` sends
//!     `body` as UTF-8 text only.
//!   - **local** (`sherpa`): transcribe in-process via sherpa-onnx; no HTTP
//!     at all. See `crate::provider::sherpa`.

pub mod openai;
pub mod sherpa;

use std::collections::HashMap;

use crate::request::TranscribeParams;

/// Mirrors `network`'s `http_request` action params — built by a cloud
/// adapter, serialized as-is into the `ActionRequest.params_json` sent to
/// `network`. Fields the adapter doesn't use stay unset (omitted from the
/// JSON) and `network` applies its own defaults.
#[derive(Debug, serde::Serialize)]
pub struct HttpRequestJson {
    pub method: &'static str,
    pub url: String,
    pub headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Base64-encoded binary request body (multipart uploads). Mutually
    /// exclusive with `body` — see `network`'s request schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    pub timeout_ms: u64,
}

/// Normalized transcription result — the shape `stt` returns to its
/// callers in `ActionResponse.data_json`, regardless of provider.
///
/// `language` is the ISO-639-1 code when known (caller-declared for cloud,
/// model language for local), else `""`. `duration_seconds` is `0` when it
/// can't be derived from the container format (e.g. an mp3/ogg upload).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptResult {
    pub text: String,
    pub language: String,
    pub duration_seconds: f32,
    pub model: String,
}

/// One transcribable model, returned by the `stt_models` action.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    /// Value the caller puts in the `model` field of `stt_transcribe`.
    pub id: String,
    /// Human-friendly label.
    pub name: String,
}

/// A cloud STT provider adapter. Local inference is intentionally NOT part
/// of this trait — it doesn't speak HTTP (see `sherpa`).
pub trait Provider {
    /// Build the `network` `http_request` params for one transcription
    /// call. `api_key` is the resolved secret value (never logged, never
    /// echoed back in any error).
    fn build_http_request(&self, params: &TranscribeParams, api_key: &str) -> HttpRequestJson;

    /// Parse the provider's raw HTTP response body (called only on 2xx)
    /// into the normalized result. `params` carries the caller's format,
    /// language hint, and model override, which shape the result.
    fn parse_response(
        &self,
        body: &[u8],
        params: &TranscribeParams,
    ) -> Result<TranscriptResult, String>;
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

/// Base64-encode bytes for the `body_base64` field of an outbound
/// `http_request`.
pub fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
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
        offset += 8 + size + (size % 2); // chunks are word-aligned
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_base64_roundtrip() {
        let bytes = [0u8, 1, 2, 0xff, 0xfe];
        use base64::Engine;
        assert_eq!(
            encode_base64(&bytes),
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
    }

    #[test]
    fn wav_duration_from_data_chunk() {
        // 16000 Hz, mono, 16-bit: 32000 bytes/sec; 64000 bytes -> 2.0 s
        let wav = crate::provider::sherpa::tests::fixture_wav(16000, 1, 64000);
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
    fn http_request_json_omits_unset_bodies() {
        let req = HttpRequestJson {
            method: "POST",
            url: "https://example.com/".into(),
            headers: HashMap::new(),
            body: None,
            body_base64: Some("AA==".into()),
            timeout_ms: 1000,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("body").is_none(), "unset body must be omitted");
        assert_eq!(json["body_base64"], "AA==");
        assert_eq!(json["timeout_ms"], 1000);
    }
}
