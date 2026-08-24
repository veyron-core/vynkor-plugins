//! Capture sessions: the background task that turns one recorder process's
//! raw PCM stdout into `AudioStreamChunk` envelopes (codec `PCM_S16LE`)
//! pushed to the requested peer, plus the registry of active sessions.
//!
//! Tasks never touch the plugin's connection — they push
//! `(target, Envelope)` pairs into the loop-owned channel
//! ([`OutboundTx`]). A session ends in exactly one of three ways, and each
//! ends with a final `end_of_stream: true` chunk so the receiving peer
//! always sees a terminated stream:
//!
//! - `mic_stop` fires the oneshot stop signal;
//! - the recorder dies (EOF / pipe error);
//! - the outbound channel closes (plugin shutting down).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use vynkor_sdk::proto::{envelope, AudioCodec, AudioStreamChunk, Envelope};

use crate::recorders::BoxedRecorder;

/// Loop-owned outbound queue: `(wire_target, envelope)` pairs produced by
/// capture tasks and forwarded verbatim by the serve loop.
pub type OutboundTx = mpsc::Sender<(String, Envelope)>;

/// Reader end of one recorder's raw PCM stdout.
pub type PcmReader = Box<dyn tokio::io::AsyncRead + Unpin + Send>;

/// Everything `mic_status` reports about one capture session.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub stream_id: u32,
    pub target: String,
    /// Backend binary that was spawned (pw-cat / parec / arecord).
    pub recorder_bin: String,
    pub device: Option<String>,
    pub rate_hz: u32,
    pub channels: u16,
    pub chunk_ms: u32,
}

pub struct ActiveSession {
    pub meta: SessionMeta,
    pub chunks_sent: Arc<AtomicU64>,
    /// Consumed once, by the first stop targeting this session.
    pub stop_tx: Option<oneshot::Sender<()>>,
    /// The recorder process handle. Kept here — not inside the capture
    /// task — so stopping kills the child synchronously, like `sound`
    /// kills players from the handler thread.
    rec: Option<BoxedRecorder>,
    task: tokio::task::JoinHandle<()>,
}

impl ActiveSession {
    pub fn new(
        meta: SessionMeta,
        chunks_sent: Arc<AtomicU64>,
        stop_tx: oneshot::Sender<()>,
        rec: BoxedRecorder,
        task: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            meta,
            chunks_sent,
            stop_tx: Some(stop_tx),
            rec: Some(rec),
            task,
        }
    }

    /// True once the capture task has exited (recorder died naturally).
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Fire the stop signal and terminate the recorder immediately. The
    /// task flushes a final `end_of_stream` chunk afterwards. Safe to call
    /// more than once.
    pub fn request_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(mut rec) = self.rec.take() {
            rec.start_kill();
        }
    }
}

/// All active capture sessions, keyed by session id.
#[derive(Default)]
pub struct State {
    next_session: u64,
    next_stream: u32,
    sessions: HashMap<String, ActiveSession>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next monotonic session id ("session-N").
    pub fn alloc_session_id(&mut self) -> String {
        self.next_session += 1;
        format!("session-{}", self.next_session)
    }

    /// Allocate a stream id for callers that didn't pin one. Starts at 1;
    /// wraps like any u32 counter would.
    pub fn alloc_stream_id(&mut self) -> u32 {
        self.next_stream = self.next_stream.wrapping_add(1).max(1);
        self.next_stream
    }

    /// Record a caller-pinned stream id so later auto-allocation skips past
    /// it — auto ids never collide with any id pinned before them.
    pub fn note_pinned_stream_id(&mut self, id: u32) {
        if id > self.next_stream {
            self.next_stream = id;
        }
    }

    pub fn insert(&mut self, session: ActiveSession) {
        self.sessions.insert(session.meta.id.clone(), session);
    }

    /// Drop sessions whose capture task already exited (recorder died).
    /// Called lazily at the top of every action — no watcher task, same
    /// reap model as `sound`.
    pub fn reap_finished(&mut self) -> Vec<String> {
        let finished: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &finished {
            self.sessions.remove(id);
        }
        finished
    }

    /// Stop one specific session. Returns false when there is none.
    pub fn stop_one(&mut self, id: &str) -> bool {
        match self.sessions.remove(id) {
            Some(mut s) => {
                s.request_stop();
                true
            }
            None => false,
        }
    }

    /// Stop every session (replace-on-start, shutdown). Returns their ids
    /// in insertion order.
    pub fn stop_all(&mut self) -> Vec<String> {
        let ids: Vec<String> = self.sessions.keys().cloned().collect();
        for (_, mut s) in self.sessions.drain() {
            s.request_stop();
        }
        ids
    }

    pub fn snapshot(&self) -> Vec<&ActiveSession> {
        let mut list: Vec<&ActiveSession> = self.sessions.values().collect();
        list.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
        list
    }
}

pub type SharedState = Arc<Mutex<State>>;

/// Bytes of s16le PCM in one chunk: rate × channels × 2 bytes/sample ×
/// chunk duration, minimum one full sample so rounding can never stall.
pub fn chunk_bytes(rate_hz: u32, channels: u16, chunk_ms: u32) -> usize {
    (rate_hz as usize)
        .saturating_mul(channels as usize)
        .saturating_mul(2)
        .saturating_mul(chunk_ms as usize)
        .saturating_div(1000)
        .max(2)
}

fn build_chunk(meta: &SessionMeta, data: Vec<u8>, end_of_stream: bool) -> Envelope {
    Envelope {
        payload: Some(envelope::Payload::AudioStreamChunk(AudioStreamChunk {
            stream_id: meta.stream_id,
            codec: AudioCodec::PcmS16le as i32,
            sample_rate: meta.rate_hz,
            channels: u32::from(meta.channels),
            data,
            end_of_stream,
        })),
        ..Default::default()
    }
}

async fn push(outbound: &OutboundTx, meta: &SessionMeta, env: Envelope) -> bool {
    matches!(outbound.send((meta.target.clone(), env)).await, Ok(()))
}

/// Drive one capture session to completion: read the recorder's raw PCM
/// stdout, frame it into fixed-size chunks, and stream them to the peer.
/// The recorder process handle stays with the session (synchronous kills);
/// this task owns only the reader. See the module docs for the termination
/// paths.
pub async fn run_capture(
    mut pcm: PcmReader,
    meta: SessionMeta,
    outbound: OutboundTx,
    mut stop_rx: oneshot::Receiver<()>,
    stats: Arc<AtomicU64>,
) {
    use tokio::io::AsyncReadExt;

    let frame = chunk_bytes(meta.rate_hz, meta.channels, meta.chunk_ms);
    let mut acc: Vec<u8> = Vec::with_capacity(frame);
    let mut buf = [0u8; 8192];

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => break,
            read = pcm.read(&mut buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    while acc.len() >= frame {
                        let data: Vec<u8> = acc.drain(..frame).collect();
                        stats.fetch_add(1, Ordering::Relaxed);
                        if !push(&outbound, &meta, build_chunk(&meta, data, false)).await {
                            return; // loop gone: plugin is shutting down
                        }
                    }
                }
            }
        }
    }

    // Final chunk carries the sub-frame remainder (trimmed to whole 16-bit
    // samples) and terminates the stream for the receiving peer.
    acc.truncate(acc.len() - (acc.len() % 2));
    let _ = push(&outbound, &meta, build_chunk(&meta, acc, true)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn meta(stream_id: u32) -> SessionMeta {
        SessionMeta {
            id: "session-1".into(),
            stream_id,
            target: "stt".into(),
            recorder_bin: "pw-cat".into(),
            device: None,
            rate_hz: 8000,
            channels: 1,
            chunk_ms: 100,
        }
    }

    #[test]
    fn chunk_bytes_math() {
        // 8 kHz mono s16le @ 100 ms = 8000 × 2 × 0.1 = 1600 bytes.
        assert_eq!(chunk_bytes(8000, 1, 100), 1600);
        // 16 kHz stereo @ 10 ms = 640 bytes.
        assert_eq!(chunk_bytes(16_000, 2, 10), 640);
        // Rounding floor: never less than one full sample.
        assert_eq!(chunk_bytes(1, 1, 1), 2);
    }

    /// Feed `total` zero bytes through the real run_capture loop via a
    /// duplex pair and collect everything that lands on the other side.
    /// The recorder side drops after writing — natural EOF, so the loop
    /// terminates the stream on its own.
    async fn drive(
        total: usize,
        chunk_cfg: (u32, u16, u32),
    ) -> (Vec<AudioStreamChunk>, Arc<AtomicU64>) {
        let (mut rec_writer, rec_reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let block = [0u8; 512];
            let mut remaining = total;
            while remaining > 0 {
                let n = remaining.min(block.len());
                rec_writer.write_all(&block[..n]).await.unwrap();
                remaining -= n;
            }
            // Drop rec_writer → reader sees EOF once drained.
        });

        let (tx, mut rx) = mpsc::channel::<(String, Envelope)>(64);
        let (_stop_tx, stop_rx) = oneshot::channel::<()>();
        let stats = Arc::new(AtomicU64::new(0));
        let m = meta(7);
        let (rate, ch, ms) = chunk_cfg;
        let m = SessionMeta {
            rate_hz: rate,
            channels: ch,
            chunk_ms: ms,
            ..m
        };

        let pcm: PcmReader = Box::new(rec_reader);
        let task = tokio::spawn(run_capture(
            pcm,
            m.clone(),
            tx.clone(),
            stop_rx,
            stats.clone(),
        ));
        drop(tx); // all pushes happen in the spawned task

        let mut chunks = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Some((target, env))) => {
                    assert_eq!(target, "stt");
                    if let Some(envelope::Payload::AudioStreamChunk(c)) = env.payload {
                        let done = c.end_of_stream;
                        chunks.push(c);
                        if done {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        (chunks, stats)
    }

    #[tokio::test]
    async fn frames_exact_chunks_with_final_remainder_eos() {
        // 2000 bytes at 1600-byte frames = 1 exact frame + 400-byte remainder.
        let (chunks, stats) = drive(2000, (8000, 1, 100)).await;
        assert!(chunks.len() >= 2, "got {} chunks", chunks.len());
        let (head, tail) = chunks.split_at(chunks.len() - 1);
        for c in head {
            assert!(!c.end_of_stream);
            assert_eq!(c.data.len(), 1600, "full frames must be exact");
        }
        let last = &tail[0];
        assert!(last.end_of_stream, "stream must terminate");
        assert_eq!(last.data.len(), 400, "remainder rides the final chunk");
        assert_eq!(last.stream_id, 7);
        assert_eq!(last.codec, AudioCodec::PcmS16le as i32);
        assert_eq!(last.sample_rate, 8000);
        assert_eq!(last.channels, 1);
        assert_eq!(stats.load(Ordering::Relaxed), head.len() as u64);
    }

    #[tokio::test]
    async fn odd_remainder_trimmed_to_whole_samples() {
        let (chunks, _stats) = drive(1601, (8000, 1, 100)).await;
        let last = chunks.last().unwrap();
        assert!(last.end_of_stream);
        assert_eq!(last.data.len(), 0, "odd byte must be trimmed away");
    }

    #[tokio::test]
    async fn empty_recorder_output_still_emits_eos() {
        let (chunks, _stats) = drive(0, (8000, 1, 100)).await;
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].end_of_stream);
        assert!(chunks[0].data.is_empty());
    }

    #[tokio::test]
    async fn stop_signal_flushes_eos_promptly() {
        let (tx, mut rx) = mpsc::channel::<(String, Envelope)>(64);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let stats = Arc::new(AtomicU64::new(0));

        let m = meta(3);
        let endless_pcm: PcmReader = Box::new(tokio::io::repeat(0u8));
        let task = tokio::spawn(run_capture(
            endless_pcm,
            m,
            tx.clone(),
            stop_rx,
            stats.clone(),
        ));
        drop(tx);

        // A few data chunks flow…
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut got_data = 0;
        while got_data < 3 && tokio::time::Instant::now() < deadline {
            if let Ok(Some((_, env))) =
                tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            {
                if let Some(envelope::Payload::AudioStreamChunk(c)) = env.payload {
                    assert!(!c.end_of_stream);
                    got_data += 1;
                }
            }
        }
        assert_eq!(got_data, 3);

        // …then the stop signal ends the stream: queued data chunks drain
        // first, then exactly one terminating chunk arrives.
        let _ = stop_tx.send(());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let (_, env) = tokio::time::timeout(deadline - tokio::time::Instant::now(), rx.recv())
                .await
                .expect("stream must terminate after stop")
                .expect("channel alive");
            match env.payload {
                Some(envelope::Payload::AudioStreamChunk(c)) if c.end_of_stream => break,
                Some(envelope::Payload::AudioStreamChunk(_)) => continue,
                other => panic!("expected audio chunk, got {other:?}"),
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    #[tokio::test]
    async fn closed_outbound_channel_exits_without_panic() {
        let (tx, rx) = mpsc::channel::<(String, Envelope)>(2);
        drop(rx); // loop already gone
        let (_stop_tx, stop_rx) = oneshot::channel::<()>();
        let m = meta(1);
        let endless_pcm: PcmReader = Box::new(tokio::io::repeat(0u8));
        let task = tokio::spawn(run_capture(
            endless_pcm,
            m,
            tx,
            stop_rx,
            Arc::new(AtomicU64::new(0)),
        ));
        let _ = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("task must exit when the channel is closed");
    }

    #[test]
    fn state_allocates_monotonic_ids() {
        let mut st = State::new();
        assert_eq!(st.alloc_session_id(), "session-1");
        assert_eq!(st.alloc_session_id(), "session-2");
        assert_eq!(st.alloc_stream_id(), 1);
        assert_eq!(st.alloc_stream_id(), 2);
    }

    #[tokio::test]
    async fn state_reap_stop_snapshot() {
        struct DeadRec;
        impl crate::recorders::RecorderProcess for DeadRec {
            fn take_stdout(&mut self) -> Option<PcmReader> {
                None
            }
            fn start_kill(&mut self) {}
        }

        let mut st = State::new();

        let mk_meta = |id: &str| SessionMeta {
            id: id.into(),
            stream_id: 1,
            target: "stt".into(),
            recorder_bin: "pw-cat".into(),
            device: None,
            rate_hz: 16000,
            channels: 1,
            chunk_ms: 100,
        };

        let still_capturing = tokio::spawn(std::future::pending());
        st.insert(ActiveSession::new(
            mk_meta("session-1"),
            Arc::new(AtomicU64::new(5)),
            {
                let (tx, _rx) = oneshot::channel::<()>();
                tx
            },
            Box::new(DeadRec),
            still_capturing,
        ));
        let already_finished = tokio::spawn(async {});
        st.insert(ActiveSession::new(
            mk_meta("session-2"),
            Arc::new(AtomicU64::new(0)),
            {
                let (tx, _rx) = oneshot::channel::<()>();
                tx
            },
            Box::new(DeadRec),
            already_finished,
        ));
        assert_eq!(st.snapshot().len(), 2);

        // Give the finished task a beat to actually exit.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let reaped = st.reap_finished();
        assert_eq!(reaped, vec!["session-2"]);
        assert_eq!(st.snapshot().len(), 1);

        assert!(st.stop_one("session-1"));
        assert!(!st.stop_one("session-999"));
        assert_eq!(st.snapshot().len(), 0);
    }
}
