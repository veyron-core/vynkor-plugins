//! Action handlers for the `mic` plugin: `mic_start`, `mic_stop`,
//! `mic_status`. All process spawning goes through
//! [`crate::recorders::RecorderSpawner`] so tests run without real audio
//! hardware.
//!
//! Capture model: `mic_start` returns as soon as the recorder is spawned —
//! PCM streams to the peer in the background. The plugin is the *single
//! owner of the mic*: starting a new session stops whatever was capturing
//! first (replace-on-start), and stopping is idempotent. Finished sessions
//! are reaped lazily at the top of every action — no watcher task.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::capture::{run_capture, ActiveSession, OutboundTx, SessionMeta, SharedState};
use crate::recorders::{
    build_args, recorder_chain, Config as RecorderConfig, RecorderProcess, RecorderSpawner,
};

// ---------------------------------------------------------------------------
// Request parsing (strict — serde validates types, not shape)
// ---------------------------------------------------------------------------

pub const SUPPORTED_FORMAT: &str = "pcm_s16le";

#[derive(Debug, Clone)]
pub struct StartRequest {
    /// Peer slug that receives the `AudioStreamChunk`s (e.g. `stt`, a
    /// client speaker slug). Required.
    pub target: String,
    /// Only s16le PCM in v0.1 — the D-12 codec every consumer decodes.
    pub format: String,
    pub device: Option<String>,
    pub sample_rate_hz: u32,
    pub num_channels: u16,
    pub chunk_ms: u32,
    /// Caller-pinned stream id; allocated from the session counter when None.
    pub stream_id: Option<u32>,
}

impl StartRequest {
    /// Strict parse with shape-naming errors; per-call params win over the
    /// operator's env defaults.
    pub fn parse(params: &Value, cfg: &RecorderConfig) -> Result<Self, String> {
        let obj = params
            .as_object()
            .ok_or_else(|| "ERR_MIC_BAD_PARAMS: params must be a JSON object".to_string())?;

        let target = match obj.get("target") {
            None | Some(Value::Null) => {
                return Err("ERR_MIC_BAD_PARAMS: 'target' is required (peer to stream \
                     the captured PCM to, e.g. \"stt\")"
                    .to_string())
            }
            Some(v) => {
                let s = v
                    .as_str()
                    .ok_or_else(|| "ERR_MIC_BAD_PARAMS: 'target' must be a string".to_string())?;
                let s = s.trim();
                if s.is_empty() {
                    return Err("ERR_MIC_BAD_PARAMS: 'target' must be non-empty".to_string());
                }
                s.to_string()
            }
        };

        let format = match obj.get("format") {
            None | Some(Value::Null) => SUPPORTED_FORMAT.to_string(),
            Some(v) => {
                let s = v
                    .as_str()
                    .ok_or_else(|| "ERR_MIC_BAD_PARAMS: 'format' must be a string".to_string())?;
                if !s.eq_ignore_ascii_case(SUPPORTED_FORMAT) {
                    return Err(format!(
                        "ERR_MIC_BAD_PARAMS: unsupported 'format' '{s}' (only \
                         \"{SUPPORTED_FORMAT}\" in v0.1)"
                    ));
                }
                SUPPORTED_FORMAT.to_string()
            }
        };

        let device = match obj.get("device") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let s = v
                    .as_str()
                    .ok_or_else(|| "ERR_MIC_BAD_PARAMS: 'device' must be a string".to_string())?;
                let s = s.trim();
                if s.is_empty() {
                    return Err("ERR_MIC_BAD_PARAMS: 'device' must be non-empty".to_string());
                }
                Some(s.to_string())
            }
        };

        let sample_rate_hz = opt_range_u32(obj, "sample_rate_hz", 8000, 192_000)?;
        let sample_rate_hz = sample_rate_hz.unwrap_or(cfg.default_rate_hz);
        let num_channels =
            opt_range_u32(obj, "num_channels", 1, 8)?.unwrap_or(cfg.default_channels as u32) as u16;
        let chunk_ms = opt_range_u32(obj, "chunk_ms", 10, 1000)?.unwrap_or(cfg.default_chunk_ms);

        let stream_id = match obj.get("stream_id") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let n = v.as_u64().ok_or_else(|| {
                    "ERR_MIC_BAD_PARAMS: 'stream_id' must be an unsigned integer".to_string()
                })?;
                if n == 0 || n > u32::MAX as u64 {
                    return Err(format!(
                        "ERR_MIC_BAD_PARAMS: 'stream_id' must be within [1, {}]",
                        u32::MAX
                    ));
                }
                Some(n as u32)
            }
        };

        Ok(Self {
            target,
            format,
            device,
            sample_rate_hz,
            num_channels,
            chunk_ms,
            stream_id,
        })
    }
}

/// Read an optional integer parameter and range-check it. Returns None for
/// absent / explicit-null keys.
fn opt_range_u32(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    min: u32,
    max: u32,
) -> Result<Option<u32>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                format!("ERR_MIC_BAD_PARAMS: '{key}' must be an unsigned integer")
            })?;
            if n < min as u64 || n > max as u64 {
                return Err(format!(
                    "ERR_MIC_BAD_PARAMS: '{key}' must be within [{min}, {max}], got {n}"
                ));
            }
            Ok(Some(n as u32))
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Spawn the capture chain's first working recorder and register a session.
///
/// The spawned task owns the process stdout; it frames PCM into chunks and
/// pushes `(target, Envelope)` pairs into `outbound`, which the serve loop
/// forwards over the connection (single-reader rule).
pub async fn handle_start(
    spawner: &dyn RecorderSpawner,
    cfg: &RecorderConfig,
    state: &SharedState,
    outbound: &OutboundTx,
    req: &StartRequest,
) -> Result<Value, String> {
    let chain = recorder_chain(cfg.recorder_override.as_deref())?;

    // Resolve the effective source device once; every argv build uses it.
    let device = req.device.clone().or_else(|| cfg.default_device.clone());

    let mut tried: Vec<String> = Vec::new();
    let mut spawned: Option<(String, Box<dyn RecorderProcess>)> = None;
    for bin in &chain {
        let args = build_args(bin, req.sample_rate_hz, req.num_channels, device.as_deref());
        match spawner.spawn(bin, &args).await {
            Ok(proc) => {
                spawned = Some((bin.clone(), proc));
                break;
            }
            Err(e) if e.contains("ERR_MIC_PROVIDER_MISSING") => tried.push(bin.clone()),
            Err(e) => return Err(e),
        }
    }

    let (recorder_bin, mut rec) = match spawned {
        Some(pair) => pair,
        None => {
            return Err(format!(
                "ERR_MIC_PROVIDER_MISSING: no working audio recorder found \
                 (tried: {}); install pw-cat (pipewire), parec (libpulse) or \
                 arecord (alsa-utils), or pin MIC_PLUGIN_RECORDER",
                tried.join(", ")
            ))
        }
    };

    // Single owner of the mic: stop whatever was capturing first.
    let replaced_ids = { state.lock().unwrap().stop_all() };

    let (session_id, stream_id, stop_tx, stop_rx) = {
        let mut st = state.lock().unwrap();
        let id = st.alloc_session_id();
        let stream_id = match req.stream_id {
            Some(pinned) => {
                st.note_pinned_stream_id(pinned);
                pinned
            }
            None => st.alloc_stream_id(),
        };
        let (tx, rx) = oneshot::channel::<()>();
        (id, stream_id, tx, rx)
    };

    let meta = SessionMeta {
        id: session_id.clone(),
        stream_id,
        target: req.target.clone(),
        recorder_bin: recorder_bin.clone(),
        device: device.clone(),
        rate_hz: req.sample_rate_hz,
        channels: req.num_channels,
        chunk_ms: req.chunk_ms,
    };
    let stats = Arc::new(AtomicU64::new(0));

    let pcm = rec
        .take_stdout()
        .ok_or_else(|| "ERR_MIC_SPAWN_FAILED: recorder produced no stdout pipe".to_string())?;
    let task = tokio::spawn(run_capture(
        pcm,
        meta.clone(),
        outbound.clone(),
        stop_rx,
        stats.clone(),
    ));

    state
        .lock()
        .unwrap()
        .insert(ActiveSession::new(meta.clone(), stats, stop_tx, rec, task));

    Ok(json!({
        "ok": true,
        "session_id": session_id,
        "stream_id": stream_id,
        "target": meta.target,
        "recorder": recorder_bin,
        "format": req.format.clone(),
        "sample_rate_hz": meta.rate_hz,
        "num_channels": meta.channels,
        "chunk_ms": meta.chunk_ms,
        "replaced": !replaced_ids.is_empty(),
    }))
}

/// Stop one specific session, or everything when `session_id` is None.
/// Idempotent: unknown / already-finished ids yield an empty list. The
/// capture task flushes the final `end_of_stream` chunk after this returns.
pub fn handle_stop(state: &SharedState, session_id: Option<&str>) -> Value {
    reap_finished(state);

    let mut st = state.lock().unwrap();
    let stopped: Vec<String> = match session_id {
        Some(id) => {
            if st.stop_one(id) {
                vec![id.to_string()]
            } else {
                Vec::new()
            }
        }
        None => st.stop_all(),
    };
    json!({ "stopped": stopped })
}

pub fn handle_status(state: &SharedState) -> Value {
    reap_finished(state);

    let st = state.lock().unwrap();
    let capturing: Vec<Value> = st
        .snapshot()
        .into_iter()
        .map(|s| {
            json!({
                "id": s.meta.id,
                "stream_id": s.meta.stream_id,
                "target": s.meta.target,
                "recorder": s.meta.recorder_bin,
                "device": s.meta.device,
                "format": SUPPORTED_FORMAT,
                "sample_rate_hz": s.meta.rate_hz,
                "num_channels": s.meta.channels,
                "chunk_ms": s.meta.chunk_ms,
                "chunks_sent": s.chunks_sent.load(std::sync::atomic::Ordering::Relaxed),
            })
        })
        .collect();
    let count = capturing.len();
    json!({ "capturing": capturing, "count": count })
}

fn reap_finished(state: &SharedState) {
    let mut st = state.lock().unwrap();
    st.reap_finished();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::State;
    use crate::recorders::{EofSpawner, FakeSpawner};
    use std::sync::Mutex;

    fn shared() -> SharedState {
        Arc::new(Mutex::new(State::new()))
    }

    fn cfg() -> RecorderConfig {
        RecorderConfig::default()
    }

    fn start_params(target: &str) -> Value {
        serde_json::json!({ "target": target, "chunk_ms": 10, "sample_rate_hz": 8000 })
    }

    async fn start(
        sp: &dyn RecorderSpawner,
        st: &SharedState,
        outbound: &OutboundTx,
        params: Value,
    ) -> Result<Value, String> {
        let req = StartRequest::parse(&params, &cfg())?;
        handle_start(sp, &cfg(), st, outbound, &req).await
    }

    #[tokio::test]
    async fn start_spawns_pwcat_and_registers_session() {
        let sp = FakeSpawner::ok(vec![0u8; 320]);
        let st = shared();
        let (outbound, _rx) = tokio::sync::mpsc::channel(16);

        let v = start(&sp, &st, &outbound, start_params("stt"))
            .await
            .unwrap();

        assert_eq!(v["ok"], true);
        assert_eq!(v["session_id"], "session-1");
        assert_eq!(v["stream_id"], 1);
        assert_eq!(v["target"], "stt");
        assert_eq!(v["recorder"], "pw-cat");
        assert_eq!(v["replaced"], false);

        let calls = sp.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "pw-cat");
        assert!(calls[0].1.contains(&"--record".to_string()));
        assert!(calls[0].1.last().unwrap() == "-");

        let status = handle_status(&st);
        assert_eq!(status["count"], 1);
        assert_eq!(status["capturing"][0]["id"], "session-1");
        assert_eq!(status["capturing"][0]["recorder"], "pw-cat");
        assert_eq!(status["capturing"][0]["target"], "stt");
    }

    #[tokio::test]
    async fn start_replaces_existing_session_and_kills_old_recorder() {
        let sp = FakeSpawner::ok(vec![0u8; 160]);
        let st = shared();
        let (outbound, _rx) = tokio::sync::mpsc::channel(64);

        start(&sp, &st, &outbound, start_params("stt"))
            .await
            .unwrap();
        let v2 = start(&sp, &st, &outbound, start_params("daemon"))
            .await
            .unwrap();

        assert_eq!(v2["session_id"], "session-2");
        assert_eq!(v2["replaced"], true);
        assert_eq!(sp.killed_bins().len(), 1, "old recorder killed once");

        let status = handle_status(&st);
        assert_eq!(status["count"], 1);
        assert_eq!(status["capturing"][0]["id"], "session-2");
    }

    #[tokio::test]
    async fn stop_by_id_all_and_unknown_id() {
        let sp = FakeSpawner::ok(vec![0u8; 160]);
        let st = shared();
        let (outbound, _rx) = tokio::sync::mpsc::channel(64);
        start(&sp, &st, &outbound, start_params("stt"))
            .await
            .unwrap();

        let v = handle_stop(&st, Some("session-999"));
        assert_eq!(v["stopped"], serde_json::json!([]));

        let v = handle_stop(&st, Some("session-1"));
        assert_eq!(v["stopped"], serde_json::json!(["session-1"]));
        assert_eq!(handle_status(&st)["count"], 0);

        // Idempotent: stopping again matches nothing.
        let v = handle_stop(&st, Some("session-1"));
        assert_eq!(v["stopped"], serde_json::json!([]));

        // Stop-all with nothing active is also empty.
        let v = handle_stop(&st, None);
        assert_eq!(v["stopped"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn natural_recorder_death_reaps_session() {
        let sp = EofSpawner::ok(vec![0u8; 320]);
        let st = shared();
        let (outbound, mut rx) = tokio::sync::mpsc::channel(64);
        start(&sp, &st, &outbound, start_params("stt"))
            .await
            .unwrap();

        // Drain until EOS arrives — the recorder EOFed after its bytes.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let (_, env) = tokio::time::timeout(deadline - tokio::time::Instant::now(), rx.recv())
                .await
                .expect("stream must terminate")
                .expect("channel alive");
            match env.payload {
                Some(vynkor_sdk::proto::envelope::Payload::AudioStreamChunk(c)) => {
                    if c.end_of_stream {
                        break;
                    }
                }
                other => panic!("expected audio chunk, got {other:?}"),
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let status = handle_status(&st);
        assert_eq!(status["count"], 0, "finished session must be reaped");
    }

    #[tokio::test]
    async fn missing_providers_fall_through_and_list_tried() {
        let missing = |b: &str| {
            Err(format!(
                "ERR_MIC_PROVIDER_MISSING: binary '{b}' not found on PATH"
            ))
        };
        let all_missing = FakeSpawner::new(
            vec![
                ("pw-cat", missing("pw-cat")),
                ("parec", missing("parec")),
                ("arecord", missing("arecord")),
            ],
            vec![],
        );
        let st = shared();
        let (outbound, _rx) = tokio::sync::mpsc::channel(16);

        let err = start(&all_missing, &st, &outbound, start_params("stt"))
            .await
            .unwrap_err();
        assert!(err.contains("ERR_MIC_PROVIDER_MISSING"), "{err}");
        for bin in ["pw-cat", "parec", "arecord"] {
            assert!(err.contains(bin), "{err}");
        }
        assert_eq!(handle_status(&st)["count"], 0);

        // pw-cat missing → falls through to parec.
        let fallthrough = FakeSpawner::new(vec![("pw-cat", missing("pw-cat"))], vec![0u8; 160]);
        let st2 = shared();
        let (outbound2, _rx2) = tokio::sync::mpsc::channel(16);
        let v = start(&fallthrough, &st2, &outbound2, start_params("stt"))
            .await
            .unwrap();
        assert_eq!(v["recorder"], "parec", "falls through to the next backend");
        assert_eq!(handle_status(&st2)["count"], 1);
    }

    #[tokio::test]
    async fn hard_spawn_failure_propagates_immediately() {
        let sp = FakeSpawner::new(
            vec![("pw-cat", Err("ERR_MIC_SPAWN_FAILED: boom".to_string()))],
            vec![],
        );
        let st = shared();
        let (outbound, _rx) = tokio::sync::mpsc::channel(16);
        let err = start(&sp, &st, &outbound, start_params("stt"))
            .await
            .unwrap_err();
        assert!(err.contains("ERR_MIC_SPAWN_FAILED"), "{err}");
        assert_eq!(sp.calls().len(), 1, "no fallthrough on non-missing errors");
    }

    #[test]
    fn parse_validation_matrix() {
        let c = cfg();
        let ok = |p: Value| StartRequest::parse(&p, &c).is_ok();
        let err = |p: Value| StartRequest::parse(&p, &c).unwrap_err();

        assert!(err(serde_json::json!(null)).contains("object"));
        assert!(err(serde_json::json!({})).contains("'target'"));
        assert!(err(serde_json::json!({"target": ""})).contains("non-empty"));
        assert!(err(serde_json::json!({"target": 5})).contains("string"));
        assert!(err(serde_json::json!({"target": "stt", "format": "opus"})).contains("format"));
        assert!(err(serde_json::json!({"target": "stt", "format": ""})).contains("format"));
        assert!(
            err(serde_json::json!({"target": "stt", "sample_rate_hz": 100}))
                .contains("sample_rate_hz")
        );
        assert!(
            err(serde_json::json!({"target": "stt", "num_channels": 0})).contains("num_channels")
        );
        assert!(err(serde_json::json!({"target": "stt", "chunk_ms": 9999})).contains("chunk_ms"));
        assert!(err(serde_json::json!({"target": "stt", "stream_id": 0})).contains("stream_id"));
        assert!(err(serde_json::json!({"target": "stt", "device": ""})).contains("device"));

        assert!(ok(start_params("stt")));

        let r = StartRequest::parse(&start_params("x"), &c).unwrap();
        assert_eq!(r.format, "pcm_s16le");
        assert_eq!(r.sample_rate_hz, 8000);
        assert_eq!(r.num_channels, 1);
        assert_eq!(r.chunk_ms, 10);
        assert!(r.stream_id.is_none());

        // Per-call params win over env defaults; defaults apply otherwise.
        let full = StartRequest::parse(
            &serde_json::json!({
                "target": "stt",
                "format": "PCM_S16LE",
                "device": "usb",
                "sample_rate_hz": 48000,
                "num_channels": 2,
                "chunk_ms": 250,
                "stream_id": 42
            }),
            &c,
        )
        .unwrap();
        assert_eq!(full.format, "pcm_s16le");
        assert_eq!(full.device.as_deref(), Some("usb"));
        assert_eq!(full.sample_rate_hz, 48000);
        assert_eq!(full.num_channels, 2);
        assert_eq!(full.chunk_ms, 250);
        assert_eq!(full.stream_id, Some(42));

        // Explicit nulls behave like absence.
        let r = StartRequest::parse(
            &serde_json::json!({"target": "stt", "device": null, "chunk_ms": null}),
            &c,
        )
        .unwrap();
        assert!(r.device.is_none());
        assert_eq!(r.chunk_ms, c.default_chunk_ms);
    }

    #[tokio::test]
    async fn caller_pinned_stream_ids_are_honored_and_unique_sessions_kept() {
        let sp = FakeSpawner::ok(vec![0u8; 160]);
        let st = shared();
        let (outbound, _rx) = tokio::sync::mpsc::channel(64);

        let p = |sid: u32| {
            serde_json::json!({
                "target": "stt", "chunk_ms": 10,
                "sample_rate_hz": 8000, "stream_id": sid
            })
        };
        let v1 = start(&sp, &st, &outbound, p(77)).await.unwrap();
        let v2 = start(&sp, &st, &outbound, p(99)).await.unwrap();
        assert_eq!(v1["stream_id"], 77);
        assert_eq!(v2["stream_id"], 99);
        assert_eq!(v2["session_id"], "session-2", "sessions stay distinct");

        let auto = start(&sp, &st, &outbound, start_params("stt"))
            .await
            .unwrap();
        assert_eq!(
            auto["stream_id"], 100,
            "auto ids skip past every id pinned before them"
        );
    }
}
