//! Opus encoding for the streaming speak path.
//!
//! sherpa-onnx synthesizes PCM (`f32` samples); the wire prefers Opus for
//! bandwidth on the network path (`AudioStreamChunk.codec == OPUS`, see
//! `vynkor_protocol.proto`). This module owns the PCM→Opus conversion: frame
//! sizing, bitrate, and the encoder state. Pure data in/out — no client, no
//! kernel, no model — so it's fully unit-testable without model files.

use opus::{Application, Bitrate, Channels, Encoder};

/// Frame length in samples per channel. 20 ms at any supported rate keeps
/// each packet small enough for one `AudioStreamChunk` (the kernel caps
/// envelope payloads at 1 MiB) while staying below Opus's 120 ms frame cap.
/// Valid Opus frame sizes: 2.5/5/10/20/40/60/80/100/120 ms.
pub const FRAME_MS: u32 = 20;

/// Default encode bitrate. 32 kbps is the conventional mono-speech default
/// for 16/24 kHz; callers can override via [`OpusConfig::bitrate`].
pub const DEFAULT_BITRATE: i32 = 32_000;

/// Opus encoder settings. Rate must be one of 8000/12000/16000/24000/48000
/// (Opus's supported rates); everything else fails at [`OpusEncoder::new`].
#[derive(Debug, Clone, Copy)]
pub struct OpusConfig {
    pub sample_rate_hz: u32,
    pub channels: u8,
    /// Bits per second; `0` = codec default.
    pub bitrate: i32,
}

impl Default for OpusConfig {
    fn default() -> Self {
        OpusConfig {
            sample_rate_hz: 24_000,
            channels: 1,
            bitrate: DEFAULT_BITRATE,
        }
    }
}

impl OpusConfig {
    pub fn validate(&self) -> Result<(), String> {
        match self.sample_rate_hz {
            8000 | 12000 | 16000 | 24000 | 48000 => {}
            other => {
                return Err(format!(
                    "unsupported opus sample rate {other} (use 8000/12000/16000/24000/48000)"
                ))
            }
        }
        if self.channels == 0 || self.channels > 2 {
            return Err(format!("opus supports 1-2 channels, got {}", self.channels));
        }
        Ok(())
    }
}

/// Samples per frame for the configured rate.
pub fn frame_samples(config: &OpusConfig) -> usize {
    (config.sample_rate_hz as usize / 1000) * FRAME_MS as usize
}

/// Encode interleaved 16-bit PCM into a sequence of Opus packets, one per
/// [`FRAME_MS`] of audio. The final (possibly short) frame is zero-padded —
/// Opus requires a full frame; padding adds ≤20 ms of silence at stream end.
/// Returns one `Vec<u8>` per frame, in order. `pcm.len()` must be divisible
/// by `channels`.
pub fn encode_pcm(pcm: &[i16], config: &OpusConfig) -> Result<Vec<Vec<u8>>, String> {
    config.validate()?;
    let channels = match config.channels {
        1 => Channels::Mono,
        2 => Channels::Stereo,
        _ => {
            return Err(format!(
                "opus supports 1-2 channels, got {}",
                config.channels
            ))
        }
    };
    if pcm.is_empty() {
        return Ok(Vec::new());
    }
    if !pcm.len().is_multiple_of(config.channels as usize) {
        return Err(format!(
            "pcm sample count {} not divisible by channel count {}",
            pcm.len(),
            config.channels
        ));
    }

    let mut encoder = Encoder::new(config.sample_rate_hz, channels, Application::Voip)
        .map_err(|e| format!("opus encoder init failed: {e}"))?;
    if config.bitrate > 0 {
        encoder
            .set_bitrate(Bitrate::Bits(config.bitrate))
            .map_err(|e| format!("opus bitrate set failed: {e}"))?;
    }

    let frame = frame_samples(config);
    let mut packets = Vec::new();
    for chunk in pcm.chunks_exact(frame * config.channels as usize) {
        let packet = encoder
            .encode_vec(chunk, 4000)
            .map_err(|e| format!("opus encode failed: {e}"))?;
        packets.push(packet);
    }
    // Zero-pad the trailing partial frame (Opus needs full frames).
    let remainder = pcm.len() % (frame * config.channels as usize);
    if remainder != 0 {
        let mut tail = vec![0i16; frame * config.channels as usize];
        tail[..remainder].copy_from_slice(&pcm[pcm.len() - remainder..]);
        let packet = encoder
            .encode_vec(&tail, 4000)
            .map_err(|e| format!("opus encode failed (tail frame): {e}"))?;
        packets.push(packet);
    }
    Ok(packets)
}

/// Decode one Opus packet back to PCM — test helper proving the encode path
/// produces decodable audio. Real decoders live on the consumer side; the
/// plugin never decodes.
#[cfg(test)]
fn decode_packet(packet: &[u8], config: &OpusConfig) -> Result<Vec<i16>, String> {
    let channels = match config.channels {
        1 => Channels::Mono,
        2 => Channels::Stereo,
        _ => return Err("bad channels".to_string()),
    };
    let mut decoder =
        opus::Decoder::new(config.sample_rate_hz, channels).map_err(|e| e.to_string())?;
    let mut out = vec![0i16; frame_samples(config) * config.channels as usize];
    let n = decoder
        .decode(packet, &mut out, false)
        .map_err(|e| e.to_string())?;
    out.truncate(n * config.channels as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, seconds: f32) -> Vec<i16> {
        let n = (rate as f32 * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (t * 440.0 * std::f32::consts::TAU).sin() * 8000.0
            })
            .map(|v| v as i16)
            .collect()
    }

    #[test]
    fn config_validates_rate() {
        let ok = OpusConfig {
            sample_rate_hz: 16_000,
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let bad = OpusConfig {
            sample_rate_hz: 44_100,
            ..Default::default()
        };
        let err = bad.validate().unwrap_err();
        assert!(err.contains("44"), "error was: {err}");
    }

    #[test]
    fn frame_samples_matches_rate() {
        let c = OpusConfig {
            sample_rate_hz: 16_000,
            ..Default::default()
        };
        assert_eq!(frame_samples(&c), 320);
        let c = OpusConfig {
            sample_rate_hz: 24_000,
            ..Default::default()
        };
        assert_eq!(frame_samples(&c), 480);
    }

    #[test]
    fn encodes_exact_frames() {
        let config = OpusConfig::default();
        let pcm = sine(config.sample_rate_hz, 1.0); // 1 s @ 24 kHz = 24000 samples
        let packets = encode_pcm(&pcm, &config).unwrap();
        assert_eq!(packets.len(), 24000 / frame_samples(&config));
        for p in &packets {
            assert!(!p.is_empty(), "every frame must produce a packet");
            assert!(p.len() <= 4000, "packet must fit the envelope cap");
        }
    }

    #[test]
    fn encodes_tail_short_frame() {
        let config = OpusConfig::default();
        let mut pcm = sine(config.sample_rate_hz, 1.0);
        pcm.truncate(24000 - 100); // drop 100 samples → short tail
        let packets = encode_pcm(&pcm, &config).unwrap();
        let full = 23900 / frame_samples(&config);
        assert_eq!(
            packets.len(),
            full + 1,
            "short tail must become one padded frame"
        );
        let decoded = decode_packet(packets.last().unwrap(), &config).unwrap();
        assert!(!decoded.is_empty());
    }

    #[test]
    fn encode_decode_roundtrip_preserves_duration() {
        let config = OpusConfig::default();
        let pcm = sine(config.sample_rate_hz, 0.5);
        let packets = encode_pcm(&pcm, &config).unwrap();
        let decoded_len: usize = packets
            .iter()
            .map(|p| decode_packet(p, &config).unwrap().len())
            .sum();
        // Each frame decodes to FRAME_MS of audio (tail padded to a full frame).
        let expected = packets.len() * frame_samples(&config) * config.channels as usize;
        assert_eq!(decoded_len, expected);
    }

    #[test]
    fn empty_pcm_yields_no_packets() {
        let packets = encode_pcm(&[], &OpusConfig::default()).unwrap();
        assert!(packets.is_empty());
    }

    #[test]
    fn rejects_odd_channel_split() {
        let config = OpusConfig {
            channels: 2,
            ..Default::default()
        };
        let err = encode_pcm(&[0i16; 5], &config).unwrap_err();
        assert!(err.contains("divisible"), "error was: {err}");
    }

    #[test]
    fn stereo_encodes() {
        let config = OpusConfig {
            channels: 2,
            ..Default::default()
        };
        // 1 s of stereo interleaved at 24 kHz.
        let pcm: Vec<i16> = (0..48000).map(|i| (i % 7) as i16).collect();
        let packets = encode_pcm(&pcm, &config).unwrap();
        assert_eq!(packets.len(), 48000 / (2 * frame_samples(&config)));
    }
}
