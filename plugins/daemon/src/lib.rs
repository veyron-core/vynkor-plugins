//! `daemon` plugin library crate: the headless always-on voice client for
//! the `agent` plugin — mic → stt → agent → tts → sound, with no business
//! logic of its own (root `ROADMAP.md`: "thin clients to `agent`").
//!
//! Every stage is an ordinary kernel-routed action call into a shipped
//! plugin (`mic`, `stt`, `agent`, `tts`, `sound`); this plugin owns only the
//! orchestration: the listen→think→speak cycle, the background turn loop and
//! its on/off state. The daemon itself declares just `PERMISSION_AUDIO`
//! (caller of the gated `mic_start`/`sound_play`) and
//! `PERMISSION_EVENT_PUBLISH` (turn events); everything it calls that is
//! ungated (`stt_listen_*`, `goal_start`, `tts_synthesize`) needs nothing.
//!
//! Outbound calls go through [`Rpc`], a channel-fronted proxy: handler and
//! timer tasks never touch the `VynkorClient` directly, because
//! `send_action` discards every non-matching inbound frame while it waits —
//! a turn started by the timer would silently eat user requests arriving
//! mid-turn. With the proxy the serve loop stays the single reader (same
//! rationale as calendar/sync-client; see `docs/PLUGIN_AUTHORING.md` §1).

pub mod request;

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use vynkor_sdk::proto::{envelope, Envelope, EventPublish};

use request::{parse_request, DaemonRequest};

/// Slug of the plugin that receives mic's PCM stream and turns it into text.
/// Fixed for v0.1: stt is the only shipped transcript provider.
pub const STT_TARGET: &str = "stt";

/// Runtime configuration (environment-driven; see `config.example.yaml`).
#[derive(Debug, Clone)]
pub struct Config {
    /// Start with the background loop enabled.
    pub enabled_at_boot: bool,
    /// Mic capture window per voice turn.
    pub turn_ms: u64,
    /// Delay between one turn's end and the next tick while enabled.
    pub gap_ms: u64,
    /// Capture rate negotiated with `stt_listen_start` and `mic_start`.
    pub sample_rate_hz: u32,
    /// mic chunk duration.
    pub chunk_ms: u32,
    /// AudioStreamChunk stream_id shared by both sides of the mic→stt hop.
    pub stream_id: i32,
    /// `tts_synthesize` provider.
    pub tts_provider: String,
    /// Provider-specific voice id.
    pub tts_voice: String,
    /// Synthesis format handed to `sound_play`.
    pub tts_format: String,
    /// `goal_start` max_steps budget per turn.
    pub max_steps: u32,
    /// Per-call timeout for mic/stt/tts/sound round-trips.
    pub timeout_ms: u32,
    /// Timeout for `goal_start` — LLM loops run long.
    pub goal_timeout_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled_at_boot: false,
            turn_ms: 6_000,
            gap_ms: 2_000,
            sample_rate_hz: 16_000,
            chunk_ms: 100,
            stream_id: 7,
            tts_provider: "sherpa".into(),
            tts_voice: "af_heart".into(),
            tts_format: "wav".into(),
            max_steps: 6,
            timeout_ms: 30_000,
            goal_timeout_ms: 120_000,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut c = Self::default();
        let read_u64 = |k: &str| -> Option<u64> {
            std::env::var(k).ok().and_then(|s| s.trim().parse::<u64>().ok())
        };
        if let Ok(v) = std::env::var("DAEMON_PLUGIN_ENABLED") {
            let v = v.trim().to_ascii_lowercase();
            c.enabled_at_boot = !v.is_empty() && v != "false" && v != "0";
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_TURN_MS") {
            c.turn_ms = v.clamp(100, 120_000);
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_GAP_MS") {
            c.gap_ms = v.clamp(50, 3_600_000);
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_SAMPLE_RATE_HZ") {
            c.sample_rate_hz = v.clamp(8_000, 192_000) as u32;
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_CHUNK_MS") {
            c.chunk_ms = v.clamp(10, 1_000) as u32;
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_STREAM_ID") {
            c.stream_id = v.max(1) as i32;
        }
        if let Ok(v) = std::env::var("DAEMON_PLUGIN_TTS_PROVIDER") {
            let v = v.trim();
            if matches!(v, "sherpa" | "openai" | "elevenlabs") {
                c.tts_provider = v.into();
            }
        }
        if let Ok(v) = std::env::var("DAEMON_PLUGIN_TTS_VOICE") {
            if !v.trim().is_empty() {
                c.tts_voice = v.trim().into();
            }
        }
        if let Ok(v) = std::env::var("DAEMON_PLUGIN_TTS_FORMAT") {
            let v = v.trim();
            if matches!(
                v,
                "wav" | "mp3" | "pcm" | "opus" | "aac" | "flac" | "ulaw"
            ) {
                c.tts_format = v.into();
            }
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_MAX_STEPS") {
            c.max_steps = v.clamp(1, 16) as u32;
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_TIMEOUT_MS") {
            c.timeout_ms = v.max(500) as u32;
        }
        if let Some(v) = read_u64("DAEMON_PLUGIN_GOAL_TIMEOUT_MS") {
            c.goal_timeout_ms = v.max(1_000) as u32;
        }
        c
    }
}

/// One pending kernel-routed call handed from a task to the serve loop,
/// which sends it and correlates the `ActionResponse` by `action_id`.
pub struct RpcCall {
    pub action: String,
    pub params_json: Vec<u8>,
    pub timeout_ms: u32,
    pub reply: oneshot::Sender<Result<Value, String>>,
}

/// Cloneable handle for kernel-routed actions into other plugins
/// (`mic`, `stt`, `agent`, `tts`, `sound`). Every [`Rpc::call`] round-trips
/// through the serve loop's single `recv()` point.
#[derive(Clone)]
pub struct Rpc {
    tx: mpsc::Sender<RpcCall>,
}

impl Rpc {
    pub fn new(tx: mpsc::Sender<RpcCall>) -> Self {
        Self { tx }
    }

    /// One kernel-routed action round-trip. Resolves to the decoded
    /// `data_json` payload on `ACTION_OK`; transport failures, non-OK
    /// statuses and timeouts all surface as `Err` naming the target action.
    pub async fn call(
        &self,
        action: &str,
        params: Value,
        timeout_ms: u32,
    ) -> Result<Value, String> {
        let params_json = serde_json::to_vec(&params)
            .map_err(|e| format!("failed to encode {action} params: {e}"))?;
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(RpcCall { action: action.to_string(), params_json, timeout_ms, reply })
            .await
            .map_err(|_| format!("{action} aborted: serve loop is shutting down"))?;
        let effective = if timeout_ms == 0 { 30_000 } else { timeout_ms };
        match tokio::time::timeout(std::time::Duration::from_millis(effective as u64), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!("{action} aborted: serve loop is shutting down")),
            Err(_) => Err(format!("{action} timed out after {effective} ms")),
        }
    }
}

/// Loop/turn state shared between the serve loop, spawned handlers and the
/// timer task. Plain atomics + small mutex-guarded slots — no `.await` while
/// holding a guard.
#[derive(Default)]
pub struct DaemonState {
    enabled: AtomicBool,
    busy: AtomicBool,
    turns_completed: AtomicU64,
    last_turn: Mutex<Option<Value>>,
}

impl DaemonState {
    pub fn new(enabled_at_boot: bool) -> Self {
        Self { enabled: AtomicBool::new(enabled_at_boot), ..Default::default() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }

    /// Claim the single turn slot: `true` exactly once until [`Self::end_turn`].
    /// Both the timer tick and `daemon_turn` go through this, so a manual
    /// turn can't overlap the loop's turn (the mic has one owner).
    pub fn try_begin_turn(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn end_turn(&self, result: &Value) {
        self.turns_completed.fetch_add(1, Ordering::SeqCst);
        *self.last_turn.lock().expect("last_turn poisoned") = Some(result.clone());
        self.busy.store(false, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> Value {
        json!({
            "enabled": self.enabled(),
            "busy": self.busy.load(Ordering::SeqCst),
            "turns_completed": self.turns_completed.load(Ordering::SeqCst),
            "last_turn":
                self.last_turn
                    .lock()
                    .expect("last_turn poisoned")
                    .clone(),
        })
    }
}

/// Best-effort event published AFTER the action response is sent (the kernel
/// namespaces the type to `plugin.daemon.turn.completed` / `.state.changed`).
#[derive(Debug)]
pub struct ChangeEvent {
    pub event_type: &'static str,
    pub payload: Value,
}

/// One handled action: the response payload plus an optional change event.
#[derive(Debug)]
pub struct ActionResult {
    pub data: Vec<u8>,
    pub event: Option<ChangeEvent>,
}

/// Handle one kernel-routed action. Stage failures inside `daemon_say` /
/// `daemon_ask` surface as `Err` → `ACTION_ERROR`; a voice turn always
/// reports through its result payload instead (`status: "error"`), because a
/// failed stage is a normal headless outcome (no speech, mic gone, agent
/// down), not a malformed request.
pub async fn handle_action(
    rpc: Rpc,
    state: std::sync::Arc<DaemonState>,
    config: &Config,
    action: &str,
    params_json: &[u8],
) -> Result<ActionResult, String> {
    match parse_request(action, params_json)? {
        DaemonRequest::Enable => {
            state.set_enabled(true);
            ok(json!({ "enabled": true }), Some(state_changed(true)))
        }
        DaemonRequest::Disable => {
            state.set_enabled(false);
            ok(json!({ "enabled": false }), Some(state_changed(false)))
        }
        DaemonRequest::Status => ok(state.snapshot(), None),
        DaemonRequest::Turn { text } => {
            if !state.try_begin_turn() {
                return Err(
                    "ERR_DAEMON_BUSY: another voice turn is already in progress".into()
                );
            }
            let result = run_voice_turn(&rpc, state.clone(), config, text).await;
            state.end_turn(&result);
            let event = ChangeEvent {
                event_type: "turn.completed",
                payload: result.clone(),
            };
            ok(result, Some(event))
        }
        DaemonRequest::Say { text } => {
            let spoken = speak(&rpc, config, &text).await?;
            ok(
                json!({
                    "spoken": true,
                    "clip_id": spoken["clip_id"],
                    "player": spoken["player"],
                    "format": config.tts_format,
                }),
                None,
            )
        }
        DaemonRequest::Ask { prompt } => {
            let answer = run_agent(&rpc, config, &prompt).await?;
            let mut spoken = false;
            if let Some(text) = answer.answer.as_deref() {
                speak(&rpc, config, text).await?;
                spoken = true;
            }
            ok(
                json!({
                    "answer": answer.answer,
                    "goal_id": answer.goal_id,
                    "goal_status": answer.goal_status,
                    "spoken": spoken,
                }),
                None,
            )
        }
    }
}

/// One listen→think→speak cycle. Never panics, never fails the caller: every
/// stage failure lands in the returned payload as `status: "error"` so the
/// background loop can run turns unattended forever.
pub async fn run_voice_turn(
    rpc: &Rpc,
    _state: std::sync::Arc<DaemonState>,
    config: &Config,
    text_override: Option<String>,
) -> Value {
    let started = Instant::now();
    let transcript = match text_override {
        Some(t) => t,
        None => match listen(rpc, config).await {
            Ok(t) => t,
            Err(e) => return turn_result("error", String::new(), None, false, started, Some(e)),
        },
    };

    if transcript.trim().is_empty() {
        return turn_result("silent", transcript, None, false, started, None);
    }

    let answer = match run_agent(rpc, config, &transcript).await {
        Ok(a) => a,
        Err(e) => return turn_result("error", transcript, None, false, started, Some(e)),
    };

    // A goal can legitimately finish without prose (declined, needs
    // confirmation, max_steps): report it rather than speaking nothing
    // silently.
    let Some(text) = answer.answer.clone() else {
        return turn_result_with_goal(
            "error",
            transcript,
            false,
            started,
            Some(format!(
                "agent finished without an answer (status: {})",
                answer.goal_status
            )),
            &answer,
        );
    };

    match speak(rpc, config, &text).await {
        Ok(_) => turn_result_with_goal("answered", transcript, true, started, None, &answer),
        Err(e) => turn_result_with_goal("error", transcript, false, started, Some(e), &answer),
    }
}

/// The listen stage: open an stt accumulation buffer, point mic at it, hold
/// the capture window, then stop both and take the transcript. `mic_stop`
/// flushes `end_of_stream` to stt BEFORE `stt_listen_stop` transcribes — the
/// ordering below is load-bearing.
async fn listen(rpc: &Rpc, config: &Config) -> Result<String, String> {
    rpc.call(
        "stt_listen_start",
        json!({
            "stream_id": config.stream_id,
            "sample_rate_hz": config.sample_rate_hz,
            "num_channels": 1,
        }),
        config.timeout_ms,
    )
    .await?;

    let mic = rpc.call(
        "mic_start",
        json!({
            "target": STT_TARGET,
            "stream_id": config.stream_id,
            "sample_rate_hz": config.sample_rate_hz,
            "num_channels": 1,
            "chunk_ms": config.chunk_ms,
        }),
        config.timeout_ms,
    )
    .await?;
    let session_id = mic["session_id"]
        .as_str()
        .ok_or_else(|| "mic_start returned no session_id".to_string())?
        .to_string();

    tokio::time::sleep(std::time::Duration::from_millis(config.turn_ms)).await;

    // Stop capturing first: mic_stop flushes end_of_stream to the peer, and
    // only then does stt have the complete buffer to transcribe.
    rpc.call("mic_stop", json!({ "session_id": session_id }), config.timeout_ms).await?;

    let stop = rpc
        .call(
            "stt_listen_stop",
            json!({ "stream_id": config.stream_id }),
            config.timeout_ms,
        )
        .await?;
    stop["text"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "stt_listen_stop returned no text".to_string())
}

/// The think stage: hand the prompt to the agent's goal loop. (Error strings
/// already name the action — the serve loop prefixes `{action} failed:` on
/// non-OK replies and [`Rpc::call`] names it on transport failures.)
async fn run_agent(rpc: &Rpc, config: &Config, prompt: &str) -> Result<AgentAnswer, String> {
    let goal = rpc
        .call(
            "goal_start",
            json!({ "goal": prompt, "max_steps": config.max_steps }),
            config.goal_timeout_ms,
        )
        .await?;
    Ok(AgentAnswer {
        goal_id: goal["id"].as_str().unwrap_or_default().to_string(),
        goal_status: goal["status"].as_str().unwrap_or_default().to_string(),
        answer: goal["final_answer"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
    })
}

struct AgentAnswer {
    goal_id: String,
    goal_status: String,
    answer: Option<String>,
}

/// The speak stage: synthesize then play. Returns `sound_play`'s response.
async fn speak(rpc: &Rpc, config: &Config, text: &str) -> Result<Value, String> {
    let synth = rpc
        .call(
            "tts_synthesize",
            json!({
                "provider": config.tts_provider,
                "voice": config.tts_voice,
                "format": config.tts_format,
                "text": text,
            }),
            config.timeout_ms,
        )
        .await?;
    let audio_base64 = synth["audio_base64"]
        .as_str()
        .ok_or_else(|| "tts_synthesize returned no audio_base64".to_string())?;
    let format = synth["format"].as_str().unwrap_or(&config.tts_format);

    rpc.call(
        "sound_play",
        json!({ "data_base64": audio_base64, "format": format }),
        config.timeout_ms,
    )
    .await
}

fn turn_result(
    status: &str,
    transcript: String,
    answer: Option<String>,
    spoken: bool,
    started: Instant,
    error: Option<String>,
) -> Value {
    json!({
        "status": status,
        "transcript": transcript,
        "answer": answer,
        "spoken": spoken,
        "duration_ms": started.elapsed().as_millis() as u64,
        "error": error,
    })
}

fn turn_result_with_goal(
    status: &str,
    transcript: String,
    spoken: bool,
    started: Instant,
    error: Option<String>,
    answer: &AgentAnswer,
) -> Value {
    json!({
        "status": status,
        "transcript": transcript,
        "answer": answer.answer,
        "goal_id": answer.goal_id,
        "goal_status": answer.goal_status,
        "spoken": spoken,
        "duration_ms": started.elapsed().as_millis() as u64,
        "error": error,
    })
}

fn ok(data: Value, event: Option<ChangeEvent>) -> Result<ActionResult, String> {
    let data =
        serde_json::to_vec(&data).map_err(|e| format!("failed to encode response: {e}"))?;
    Ok(ActionResult { data, event })
}

fn state_changed(enabled: bool) -> ChangeEvent {
    ChangeEvent {
        event_type: "state.changed",
        payload: json!({ "enabled": enabled }),
    }
}

/// Build the outbound `EventPublish` envelope for a change event.
pub fn event_envelope(event: &ChangeEvent) -> Envelope {
    Envelope {
        payload: Some(envelope::Payload::EventPublish(EventPublish {
            event_type: event.event_type.to_string(),
            payload_json: event.payload.to_string().into_bytes(),
        })),
        ..Default::default()
    }
}
