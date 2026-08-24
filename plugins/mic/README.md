# mic plugin

Microphone capture primitive for vynkor plugins — the single owner of the
mic. `mic_start` spawns a well-known host recorder binary directly with
argv (never a shell), reads raw PCM from its stdout in a background task,
and streams it out as `AudioStreamChunk` envelopes (codec `PCM_S16LE`) to
the requested peer — D-12 machinery, `tts_speak` in reverse.
`mic_stop` ends a session (idempotent), `mic_status` reports what is
capturing. Declares `PERMISSION_AUDIO`, `PERMISSION_AUDIO_STREAM`, and
`PERMISSION_IPC_SEND`.

## Status

v0.1.0 — `mic_start` / `mic_stop` / `mic_status`. Local-only, offline.

**See [`USAGE.md`](./USAGE.md)** for the caller-facing guide: the
start/stop capture model, full request/response reference, what the
receiving peer sees on the wire, the operator gating checklist
(`PERMISSION_AUDIO` + `MIC_PLUGIN_IPC_TARGETS`), every error message a
caller can hit, and common patterns (voice→text via `stt`, streaming to
WS clients, replace-on-start semantics).

## Providers

| Chain position | Binary | Source of PCM |
|---|---|---|
| 1 | `pw-cat --record --raw` | PipeWire (`--target=<dev>`) |
| 2 | `parec` | PulseAudio (`--device=<dev>`) |
| 3 | `arecord -t raw -f S16_LE` | bare ALSA (`-D <dev>`) |

A missing binary falls through to the next in the chain; all missing →
`ERR_MIC_PROVIDER_MISSING` listing what was tried. Pin one backend with
`MIC_PLUGIN_RECORDER` (only the known three may be pinned).

All backends emit headerless s16le PCM on stdout at the requested rate and
channel count — that is exactly the D-12 codec `PCM_S16LE` that `stt`'s
listen path and the clients decode.

## Actions

| Action | Params | Result |
|---|---|---|
| `mic_start` | required `target`; optional `format` (`pcm_s16le` only), `device`, `sample_rate_hz` (8000–192000, default 16000), `num_channels` (1–8, default 1), `chunk_ms` (10–1000, default 100), `stream_id` | `{ ok, session_id, stream_id, target, recorder, format, sample_rate_hz, num_channels, chunk_ms, replaced }` — returns as soon as the recorder is spawned |
| `mic_stop` | optional `session_id` (omit = stop everything) | `{ stopped: [ids] }` — idempotent; the final `end_of_stream` chunk is flushed to the peer after this response |
| `mic_status` | — | `{ capturing: [{id, stream_id, target, recorder, device, format, sample_rate_hz, num_channels, chunk_ms, chunks_sent}], count }` |

Capture model:

- **Non-blocking**: `mic_start` returns once the recorder process exists;
  capture continues in the background.
- **Single owner of the mic**: starting a new session stops whatever was
  capturing first (`replaced: true`). No mixing, no parallel captures.
- **Streaming**: PCM is framed into fixed-size chunks (`chunk_ms` worth of
  bytes) and pushed as `AudioStreamChunk{codec: PCM_S16LE}` envelopes to
  `target`. A sub-frame remainder rides the final chunk, which carries
  `end_of_stream: true` — peers always see a terminated stream, whether
  capture ended by `mic_stop`, recorder death, or plugin shutdown.
- **Reaping** happens lazily on every action — a session whose recorder
  died disappears from status on the next interaction; no watcher task.
- Shutdown stops all sessions best-effort; recorders spawn with stdin/stderr
  null-routed and `kill_on_drop`, so no zombies outlive the plugin.

Typical consumer flow (D-12 voice pipeline): open a listen buffer on `stt`
(`stt_listen_start`), then call `mic_start {"target": "stt", ...}` — the
captured PCM streams straight into the transcription buffer, and
`stt_listen_stop` transcribes it after `end_of_stream` arrives.

## Error taxonomy

`ERR_MIC_BAD_PARAMS` / `PROVIDER_MISSING` / `SPAWN_FAILED`.

## Security model

- argv-only spawn of well-known binaries; no shell, so device names and
  numeric parameters are never interpreted.
- Captured audio is sent only to the peer named in `mic_start.target`,
  which is gated twice kernel-side: the caller needs `PERMISSION_AUDIO`
  to drive the mic's actions, and the plugin may unicast only to slugs on
  its operator-managed `MIC_PLUGIN_IPC_TARGETS` allowlist (T-04,
  default-deny — unset means nothing can be streamed anywhere).
- No file I/O, no network egress, no temp files: fully offline
  (`sandbox: true` is safe).

## Config

Config is read from the environment (the kernel's plugin supervisor
translates the `config.yaml` `env:` entry into these before spawning the
plugin).

| Env var | Default | Meaning |
|---|---|---|
| `MIC_PLUGIN_RECORDER` | *(unset)* | Pin one backend binary (skips the chain fallthrough) |
| `MIC_PLUGIN_DEVICE` | *(unset)* | Default capture device; per-call `device` param wins |
| `MIC_PLUGIN_RATE` | `16000` | Default sample rate; per-call `sample_rate_hz` wins |
| `MIC_PLUGIN_CHANNELS` | `1` | Default channel count; per-call `num_channels` wins |
| `MIC_PLUGIN_CHUNK_MS` | `100` | Default chunk duration; per-call `chunk_ms` wins |
| `MIC_PLUGIN_IPC_TARGETS` | *(unset)* | Comma-separated T-04 allowlist of streaming targets |

## Testing

`cargo test` — unit tests cover request validation, chain selection, exact
argv construction, chunk framing math (exact frames, remainder riding the
final chunk, odd-byte trim), stop/EOS semantics, replace-on-start, provider
fallthrough, and lazy reaping — plus fake-kernel end-to-end tests over a
real socket pair (`UnixStream::pair`) driving the actual serve loop: PCM
chunks arrive addressed to the peer with correct codec/rate/stream_id,
stop flushes `end_of_stream`, natural recorder EOF terminates the stream,
replace-on-start reports `replaced`. CI never touches real audio hardware
(recorders are injected fakes).
