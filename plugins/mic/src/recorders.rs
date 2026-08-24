//! Audio recorder backends: `pw-cat --record` (PipeWire), `parec`
//! (PulseAudio), `arecord` (ALSA). Every capture session spawns the binary
//! directly with argv — never a shell — so a crafted device name or format
//! string cannot inject commands. All three emit raw PCM (s16le) on stdout,
//! which the capture loop ([`crate::capture`]) reads and frames into
//! `AudioStreamChunk`s.
//!
//! The [`RecorderSpawner`] trait is the process-execution boundary: tests
//! inject a fake so CI never touches real audio hardware or host binaries.

use std::io::ErrorKind;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::{Child, Command};

/// Operator env var: pin one backend binary instead of falling through the chain.
pub const RECORDER_ENV: &str = "MIC_PLUGIN_RECORDER";
/// Operator env var: default capture device; a per-call `device` param wins.
pub const DEVICE_ENV: &str = "MIC_PLUGIN_DEVICE";
/// Operator env var: default sample rate in Hz; a per-call param wins.
pub const RATE_ENV: &str = "MIC_PLUGIN_RATE";
/// Operator env var: default channel count; a per-call param wins.
pub const CHANNELS_ENV: &str = "MIC_PLUGIN_CHANNELS";
/// Operator env var: default chunk duration in ms; a per-call param wins.
pub const CHUNK_MS_ENV: &str = "MIC_PLUGIN_CHUNK_MS";

pub const DEFAULT_RATE_HZ: u32 = 16_000;
pub const DEFAULT_CHANNELS: u16 = 1;
pub const DEFAULT_CHUNK_MS: u32 = 100;

/// Operator policy resolved once from env (constructed directly in tests).
#[derive(Debug, Clone)]
pub struct Config {
    /// `MIC_PLUGIN_RECORDER`: pin one backend binary.
    pub recorder_override: Option<String>,
    /// `MIC_PLUGIN_DEVICE`: default capture device; per-call param wins.
    pub default_device: Option<String>,
    /// `MIC_PLUGIN_RATE`: default sample rate; per-call param wins.
    pub default_rate_hz: u32,
    /// `MIC_PLUGIN_CHANNELS`: default channels; per-call param wins.
    pub default_channels: u16,
    /// `MIC_PLUGIN_CHUNK_MS`: default chunk duration; per-call param wins.
    pub default_chunk_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            recorder_override: None,
            default_device: None,
            default_rate_hz: DEFAULT_RATE_HZ,
            default_channels: DEFAULT_CHANNELS,
            default_chunk_ms: DEFAULT_CHUNK_MS,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let parse_num = |v: Option<String>, min: i64, max: i64, dflt: u32| -> u32 {
            v.and_then(|s| s.trim().parse::<i64>().ok())
                .filter(|&n| n >= min && n <= max)
                .map(|n| n as u32)
                .unwrap_or(dflt)
        };
        Self {
            recorder_override: env(RECORDER_ENV)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            default_device: env(DEVICE_ENV)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            default_rate_hz: parse_num(env(RATE_ENV), 8000, 192_000, DEFAULT_RATE_HZ),
            // Channels cap mirrors sound-card reality; anything above is a typo.
            default_channels: parse_num(env(CHANNELS_ENV), 1, 8, DEFAULT_CHANNELS as u32) as u16,
            default_chunk_ms: parse_num(env(CHUNK_MS_ENV), 10, 1000, DEFAULT_CHUNK_MS),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider chain and argv construction
// ---------------------------------------------------------------------------

const KNOWN_RECORDERS: [&str; 3] = ["pw-cat", "parec", "arecord"];

/// True when `bin` is one of the built-in backends.
pub fn is_known_recorder(bin: &str) -> bool {
    KNOWN_RECORDERS.contains(&bin)
}

/// Ordered candidate binaries for one capture request.
///
/// Auto mode: `pw-cat --record` → `parec` → `arecord` — all three accept
/// rate/channels/format and a source device, so no capability filtering is
/// needed on this side of the sound/mic mirror.
///
/// Override mode (`MIC_PLUGIN_RECORDER`): exactly that binary is used. Only
/// the known three may be pinned — an unknown recorder would need unknown
/// flags, and unlike playback there is no sane pass-through argv for it.
pub fn recorder_chain(override_bin: Option<&str>) -> Result<Vec<String>, String> {
    match override_bin {
        Some(bin) if is_known_recorder(bin) => Ok(vec![bin.to_string()]),
        Some(bin) => Err(format!(
            "ERR_MIC_BAD_PARAMS: MIC_PLUGIN_RECORDER='{bin}' is not a known \
             recorder (expected one of: {})",
            KNOWN_RECORDERS.join(", ")
        )),
        None => Ok(KNOWN_RECORDERS.iter().map(|b| b.to_string()).collect()),
    }
}

/// Build argv for one backend. The `-` (stdout) sink is ALWAYS the last
/// argument — same convention as `sound`, where the file path goes last.
/// Raw s16le output so the capture loop gets headerless PCM.
pub fn build_args(
    recorder: &str,
    rate_hz: u32,
    channels: u16,
    device: Option<&str>,
) -> Vec<String> {
    match recorder {
        // `--raw` = headerless PCM on stdout ("-").
        "pw-cat" => {
            let mut args = vec![
                "--record".to_string(),
                "--raw".to_string(),
                "--format=s16le".to_string(),
                format!("--rate={rate_hz}"),
                format!("--channels={channels}"),
            ];
            if let Some(d) = device {
                args.push(format!("--target={d}"));
            }
            args.push("-".to_string());
            args
        }
        "parec" => {
            let mut args = vec![
                format!("--rate={rate_hz}"),
                format!("--channels={channels}"),
                "--format=s16le".to_string(),
            ];
            if let Some(d) = device {
                args.push(format!("--device={d}"));
            }
            args.push("-".to_string());
            args
        }
        "arecord" => {
            // `-q`: no progress noise; `-t raw`: headerless output.
            let mut args = vec![
                "-q".to_string(),
                "-t".to_string(),
                "raw".to_string(),
                "-f".to_string(),
                "S16_LE".to_string(),
                "-r".to_string(),
                rate_hz.to_string(),
                "-c".to_string(),
                channels.to_string(),
            ];
            if let Some(d) = device {
                args.extend(["-D".to_string(), d.to_string()]);
            }
            args.push("-".to_string());
            args
        }
        // Unreachable via recorder_chain, but never panic in prod paths:
        // an unpinned unknown binary just gets the stdout sink.
        _ => vec!["-".to_string()],
    }
}

// ---------------------------------------------------------------------------
// Process execution boundary
// ---------------------------------------------------------------------------

/// Handle to one spawned capture process.
pub trait RecorderProcess: Send {
    /// Take the child's stdout (raw PCM stream). Returns None if already taken.
    fn take_stdout(&mut self) -> Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>;
    /// Terminate the process (best-effort; safe to call more than once).
    fn start_kill(&mut self);
}

pub type BoxedRecorder = Box<dyn RecorderProcess>;

/// Process execution boundary — mocked in tests so CI never touches a real
/// audio stack.
#[async_trait]
pub trait RecorderSpawner: Send + Sync {
    /// Spawn `bin args` with piped stdout. Errors carry the ERR_MIC_* taxonomy:
    /// PROVIDER_MISSING when the binary isn't installed (the caller falls
    /// through to the next candidate), SPAWN_FAILED otherwise.
    async fn spawn(&self, bin: &str, args: &[String]) -> Result<BoxedRecorder, String>;
}

pub struct RealSpawner;

#[async_trait]
impl RecorderSpawner for RealSpawner {
    async fn spawn(&self, bin: &str, args: &[String]) -> Result<BoxedRecorder, String> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Dropping the handle after a kill reaps the child instead of
            // leaving a zombie until plugin exit.
            .kill_on_drop(true);

        match cmd.spawn() {
            Ok(child) => Ok(Box::new(RealProcess { child })),
            Err(e) if e.kind() == ErrorKind::NotFound => Err(format!(
                "ERR_MIC_PROVIDER_MISSING: binary '{bin}' not found on PATH"
            )),
            Err(e) => Err(format!("ERR_MIC_SPAWN_FAILED: spawn '{bin}' failed: {e}")),
        }
    }
}

struct RealProcess {
    child: Child,
}

impl RecorderProcess for RealProcess {
    fn take_stdout(&mut self) -> Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>> {
        self.child.stdout.take().map(|out| Box::new(out) as _)
    }

    fn start_kill(&mut self) {
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Test double: records every invocation, returns canned results keyed by
/// binary name (unlisted binaries succeed). The fake process streams
/// `pcm_bytes` out of its stdout then holds open until killed — mirroring a
/// real recorder that emits silence forever.
#[cfg(test)]
pub struct FakeSpawner {
    results: std::sync::Mutex<std::collections::HashMap<String, Result<(), String>>>,
    calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    kill_log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pub pcm_bytes: Vec<u8>,
}

#[cfg(test)]
impl FakeSpawner {
    /// Every spawn succeeds; the recorder emits `pcm_bytes` then blocks.
    pub fn ok(pcm_bytes: Vec<u8>) -> Self {
        Self::new(Vec::new(), pcm_bytes)
    }

    /// Per-binary outcomes; unlisted binaries succeed.
    pub fn new(results: Vec<(&str, Result<(), String>)>, pcm_bytes: Vec<u8>) -> Self {
        Self {
            results: std::sync::Mutex::new(
                results
                    .into_iter()
                    .map(|(b, r)| (b.to_string(), r))
                    .collect(),
            ),
            calls: std::sync::Mutex::new(Vec::new()),
            kill_log: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            pcm_bytes,
        }
    }

    /// Spawn invocations recorded by [`RecorderSpawner::spawn`] — consumed
    /// by the lib-target handler tests; unused in the bin target.
    #[allow(dead_code)]
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }

    pub fn killed_bins(&self) -> Vec<String> {
        self.kill_log.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl RecorderSpawner for FakeSpawner {
    async fn spawn(&self, bin: &str, args: &[String]) -> Result<BoxedRecorder, String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.to_string(), args.to_vec()));
        let outcome = self
            .results
            .lock()
            .unwrap()
            .get(bin)
            .cloned()
            .unwrap_or(Ok(()));
        match outcome {
            Ok(()) => {
                let (tx, rx) = tokio::io::duplex(4096);
                let pcm = self.pcm_bytes.clone();
                let kill_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let holder_flag = std::sync::Arc::clone(&kill_flag);
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let mut writer = tx;
                    let _ = writer.write_all(&pcm).await;
                    // A real recorder keeps streaming after its first buffer;
                    // hold the pipe open until killed so EOF only happens via
                    // start_kill (EofSpawner is the natural-EOF variant).
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        if holder_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                    }
                });
                Ok(Box::new(FakeProcess {
                    kill_log: std::sync::Arc::clone(&self.kill_log),
                    bin: bin.to_string(),
                    kill_flag,
                    stdout: Some(Box::new(rx)),
                }))
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
struct FakeProcess {
    bin: String,
    kill_log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    kill_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stdout: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
}

#[cfg(test)]
impl RecorderProcess for FakeProcess {
    fn take_stdout(&mut self) -> Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>> {
        self.stdout.take()
    }

    fn start_kill(&mut self) {
        self.kill_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.kill_log.lock().unwrap().push(self.bin.clone());
    }
}

/// Test double variant whose recorder closes its stdout right after the
/// canned bytes — simulates a recorder dying mid-session (natural EOF).
#[cfg(test)]
pub struct EofSpawner {
    calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    kill_log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pcm_bytes: Vec<u8>,
}

#[cfg(test)]
impl EofSpawner {
    pub fn ok(pcm_bytes: Vec<u8>) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            kill_log: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            pcm_bytes,
        }
    }

    #[allow(dead_code)]
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    pub fn killed_bins(&self) -> Vec<String> {
        self.kill_log.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl RecorderSpawner for EofSpawner {
    async fn spawn(&self, bin: &str, args: &[String]) -> Result<BoxedRecorder, String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.to_string(), args.to_vec()));
        // Writer side writes the canned PCM then drops — the reader sees
        // clean EOF once everything is drained.
        let (tx, rx) = tokio::io::duplex(4096);
        let pcm = self.pcm_bytes.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut writer = tx;
            let _ = writer.write_all(&pcm).await;
        });
        Ok(Box::new(EofProcess {
            bin: bin.to_string(),
            kill_log: std::sync::Arc::clone(&self.kill_log),
            kill_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stdout: Some(Box::new(rx)),
        }))
    }
}

#[cfg(test)]
struct EofProcess {
    bin: String,
    kill_log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    kill_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stdout: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
}

#[cfg(test)]
impl RecorderProcess for EofProcess {
    fn take_stdout(&mut self) -> Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>> {
        self.stdout.take()
    }

    fn start_kill(&mut self) {
        self.kill_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.kill_log.lock().unwrap().push(self.bin.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_chain_is_pwcat_parec_arecord() {
        assert_eq!(
            recorder_chain(None).unwrap(),
            vec!["pw-cat", "parec", "arecord"]
        );
    }

    #[test]
    fn override_pins_known_backend() {
        assert_eq!(recorder_chain(Some("arecord")).unwrap(), vec!["arecord"]);
    }

    #[test]
    fn override_rejects_unknown_backend() {
        let err = recorder_chain(Some("my-recorder")).unwrap_err();
        assert!(err.contains("ERR_MIC_BAD_PARAMS"), "{err}");
        assert!(err.contains("known recorder"), "{err}");
        assert!(err.contains("pw-cat") && err.contains("arecord"), "{err}");
    }

    #[test]
    fn args_pwcat_record_raw_then_stdout_last() {
        let args = build_args("pw-cat", 16000, 1, None);
        assert_eq!(
            args,
            vec![
                "--record",
                "--raw",
                "--format=s16le",
                "--rate=16000",
                "--channels=1",
                "-"
            ]
        );
        // Device lands before the stdout sink.
        let args = build_args("pw-cat", 44100, 2, Some("usb_mic"));
        assert_eq!(args[args.len() - 1], "-");
        assert_eq!(args[args.len() - 2], "--target=usb_mic");
        assert!(args.contains(&"--rate=44100".to_string()));
        assert!(args.contains(&"--channels=2".to_string()));
    }

    #[test]
    fn args_parec_device_flag() {
        let args = build_args("parec", 48000, 2, Some("alsa_input.usb"));
        assert_eq!(
            args,
            vec![
                "--rate=48000",
                "--channels=2",
                "--format=s16le",
                "--device=alsa_input.usb",
                "-"
            ]
        );
    }

    #[test]
    fn args_arecord_raw_s16le_with_device() {
        let args = build_args("arecord", 8000, 1, Some("hw:0"));
        assert_eq!(
            args,
            vec!["-q", "-t", "raw", "-f", "S16_LE", "-r", "8000", "-c", "1", "-D", "hw:0", "-"]
        );
        let bare = build_args("arecord", 16000, 1, None);
        assert_eq!(bare[bare.len() - 1], "-");
        assert!(!bare.contains(&"-D".to_string()));
    }

    #[test]
    fn args_unknown_recorder_still_ends_with_stdout_sink() {
        assert_eq!(build_args("my-recorder", 16000, 1, None), vec!["-"]);
    }

    #[tokio::test]
    async fn real_spawner_missing_binary_maps_to_provider_missing() {
        let err = match RealSpawner
            .spawn("definitely-not-a-real-audio-bin-xyz", &["-".to_string()])
            .await
        {
            Ok(_) => panic!("spawn of a nonexistent binary must fail"),
            Err(e) => e,
        };
        assert!(err.contains("ERR_MIC_PROVIDER_MISSING"), "{err}");
    }

    #[tokio::test]
    async fn real_spawner_streams_stdout_pipe() {
        // `cat` reading /dev/zero is a real piped process on any Linux box.
        let mut proc = match RealSpawner
            .spawn(
                "head",
                &["-c".to_string(), "64".to_string(), "/dev/zero".to_string()],
            )
            .await
        {
            Ok(p) => p,
            Err(_) => return, // exotic CI without coreutils: skip quietly
        };
        use tokio::io::AsyncReadExt;
        let mut out = proc.take_stdout().expect("stdout must be pipeable");
        let mut buf = vec![0u8; 128];
        let n = out.read(&mut buf).await.unwrap();
        assert_eq!(n, 64);
        proc.start_kill();
    }

    #[test]
    fn config_defaults_match_sound_mirror() {
        let cfg = Config::default();
        assert_eq!(cfg.default_rate_hz, 16_000);
        assert_eq!(cfg.default_channels, 1);
        assert_eq!(cfg.default_chunk_ms, 100);
        assert!(cfg.recorder_override.is_none());
        assert!(cfg.default_device.is_none());
    }
}
