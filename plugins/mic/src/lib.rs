//! `mic` plugin — the single owner of the microphone.
//!
//! Audio capture primitive: `mic_start` spawns a well-known host recorder
//! binary directly with argv — never a shell — reads raw PCM from its
//! stdout in a background task, and streams it out as
//! [`AudioStreamChunk`]s (codec `PCM_S16LE`) to the requested peer —
//! D-12 machinery, `tts_speak` in reverse. `mic_stop` ends a session
//! (idempotent), `mic_status` reports what is capturing.
//! Provider chain: `pw-cat --record` → `parec` → `arecord`. Same delivery
//! model as `sound` / `clipboard` / `notify`: argv-only spawn of host
//! binaries.
//!
//! Loop shape (docs/PLUGIN_AUTHORING.md §1): the serve loop is the single
//! reader of the connection and owns the client exclusively; capture tasks
//! never touch it — they push `(target, Envelope)` pairs into an mpsc
//! channel the loop drains via `tokio::select!`.
//!
//! Scope and non-goals: see ROADMAP.md. All logic lives in the `recorders`,
//! `capture`, and `handler` modules; `main.rs` only wires the loop.

pub mod capture;
pub mod handler;
pub mod recorders;
