# daemon plugin

The headless always-on voice client for the [`agent`](../agent/) plugin:
mic → stt → agent → tts → sound, with no business logic of its own (root
`ROADMAP.md`: "thin clients to `agent` — no business logic of their own,
just UI surface over the same kernel API"; the daemon's surface is the
absence of one — no window, no browser, just a listen loop and a speaker).

Every stage is an ordinary kernel-routed action call into a shipped plugin:

| Stage | Provider | Call |
|---|---|---|
| capture | [`mic`](../mic/) | `mic_start` (PCM streamed peer-to-peer to `stt`) |
| transcribe | [`stt`](../stt/) | `stt_listen_start`/`stt_listen_stop` → transcript |
| think | [`agent`](../agent/) | `goal_start` → `final_answer` |
| synthesize | [`tts`](../tts/) | `tts_synthesize` → audio bytes |
| play | [`sound`](../sound/) | `sound_play` (`data_base64`) |

The daemon owns only the orchestration: the voice turn, the background turn
loop and its on/off state. Restart it, kill it, run it under a strict
supervisor resource limit — nothing else depends on its process.

## Permissions

The daemon declares exactly what it holds as a *caller*:

- `PERMISSION_AUDIO` — T-19 anti-laundering: `mic_start` and `sound_play`
  are gated actions, so their caller must hold their permission.
- `PERMISSION_EVENT_PUBLISH` — best-effort `turn.completed` /
  `state.changed` events.

Everything else it calls is ungated (`stt_listen_*`, `goal_start`,
`tts_synthesize`). It declares no `ipc_targets` — all calls are
kernel-routed actions, not raw frame forwarding (T-04 does not apply; same
as `notify` calling `tts_synthesize`).

## Actions

| Action | Params | Result |
|---|---|---|
| `daemon_enable` | `{}` | `{enabled: true}` — start the background listen loop |
| `daemon_disable` | `{}` | `{enabled: false}` — stop the loop (an in-flight turn finishes) |
| `daemon_status` | `{}` | `{enabled, busy, capturing, mode, turns_completed, last_turn}` |
| `daemon_turn` | `{text?}` | run one voice cycle now; `text` skips mic/stt. See below |
| `daemon_say` | `{text}` | synthesize + play text through tts + sound |
| `daemon_ask` | `{prompt}` | agent round-trip; the answer is spoken aloud and returned |

### Listen modes (`DAEMON_PLUGIN_MODE`)

How a turn decides the user stopped talking:

- **`window`** (default) — hold the mic for a fixed `DAEMON_PLUGIN_TURN_MS`
  and take whatever was said. v0.1 behavior; predictable but rigid.
- **`vad`** — open-ended capture: the turn ends when `stt` publishes its
  speech-ended event (energy VAD behind `STT_PLUGIN_VAD=on`), i.e. when
  you actually stop talking. Caps keep it safe:
  `DAEMON_PLUGIN_VAD_WAIT_MS` (no speech at all → turn ends `silent`),
  `DAEMON_PLUGIN_VAD_MAX_UTTERANCE_MS` (endpoint never fired → cut and
  transcribe anyway). Enable once and talk whenever — "walk & talk".
- **`ptt`** — push-to-talk with the [`hotkey`](../hotkey/) plugin: idle
  until `hotkey_pressed` (matching `DAEMON_PLUGIN_PTT_BINDING`), capture
  while held, end on `hotkey_released`, then think+speak as usual.
  `DAEMON_PLUGIN_PTT_MAX_HOLD_MS` auto-releases a stuck key. Manual
  `daemon_turn` in ptt mode falls back to window behavior — nobody is
  holding a key programmatically.

All three modes share one busy slot (the mic has one owner) and produce
identical `turn.completed` events.

### The voice turn

One cycle of [`run_voice_turn`](src/lib.rs):

1. `stt_listen_start` opens an accumulation buffer on `stream_id`.
2. `mic_start {target: "stt", stream_id}` points the mic at it.
3. The endpoint fires per mode: fixed window elapses / stt reports
   silence-after-speech / hotkey released.
4. `mic_stop` first — it flushes `end_of_stream` to stt — then
   `stt_listen_stop` transcribes the complete buffer.
5. Empty transcript → done, `status: "silent"`. Nothing else runs.
6. `goal_start {goal: transcript}` → `final_answer`. A goal that ends
   without prose (`declined`, `needs_confirmation`, `max_steps_reached`)
   reports `status: "error"` naming the goal status rather than speaking
   nothing silently.
7. `tts_synthesize` → `sound_play` with the synthesized bytes.

The result publishes as a `turn.completed` event:

```json
{"status": "answered", "transcript": "what time is it",
 "answer": "It is noon.", "spoken": true, "duration_ms": 8120, "error": null}
```

`status` is `answered` | `silent` | `error`. Stage failures land in the
payload (`status: "error"`, `error: "<stage> failed: …"`), never as
ACTION_ERROR — a failed stage is a normal unattended-daemon outcome, not a
malformed request. If `mic_start` fails after `stt_listen_start`, the
half-open buffer is still discarded (fail closed).

### Failure semantics per action

- `daemon_say` / `daemon_ask` fail as ACTION_ERROR when any stage fails —
  the caller is waiting on the outcome.
- `daemon_turn` always answers ACTION_OK with a `status` field — the caller
  asked for a cycle to be *run*, and it was.
- One turn at a time: `daemon_turn` while a turn is running →
  `ERR_DAEMON_BUSY`; the timer tick simply skips while busy. The single slot
  exists because the mic has one owner.

## Events

Both are best-effort, published after the response (database's contract):

- `plugin.daemon.turn.completed` — every finished turn (see above), from
  manual `daemon_turn`, the background loop, and ptt turns alike.
- `plugin.daemon.state.changed` — `{"enabled": true|false}` on enable/disable.

Inbound, the daemon subscribes to `stt_speech_started`/`_ended` (vad mode)
and `hotkey_pressed`/`hotkey_released` (ptt mode); subscribing in window
mode is harmless — events arrive and drop unread.

## Concurrency architecture

Single-reader select loop + channel-fronted RPC proxy + spawned tasks —
the calendar/sync-client pattern (`docs/PLUGIN_AUTHORING.md` §1). The serve
loop exclusively owns the `VynkorClient`; handler tasks, the timer-driven
turn task and the ptt event task reach mic/stt/agent/tts/sound through the
`Rpc` proxy channel, so a turn started by a tick can never eat an inbound
user request (`send_action`'s discard-while-waiting would). Kernel events
forward onto an in-process broadcast bus that the vad listen stage and the
ptt task subscribe to. Loop state (enabled/busy/capturing/counter/last
turn) lives behind atomics + small mutex slots in
[`DaemonState`](src/lib.rs) — no `.await` while holding a guard.

## Testing

`cargo test` — 9 unit tests over request parsing/validation plus 20 e2e
tests driving the real serve loop against a fake kernel over
`UnixStream::pair` (registration handshake asserting the declared
permissions, scripted mic/stt/agent/tts/sound stand-ins recording every
outbound call in arrival order, kernel-event injection for the vad/ptt
feeds): full-pipeline order per mode, text-bypass turn, silent turn,
vad-mode endpointing (speech-ended ends the turn; no speech → clean
timeout), ptt press/release lifecycle, disabled/wrong-binding presses
ignored, stuck-key max-hold release freeing the busy slot, per-stage
failure routing (payload vs ACTION_ERROR), busy-slot rejection, and
enable/disable loop ticks. No live kernel, no audio hardware.

## Status

v0.2. Depends on `mic`, `stt`, `agent`, `tts` and `sound` being registered;
the daemon degrades per-stage (see `USAGE.md` errors) but needs all five for
a full answered turn. vad mode additionally wants `STT_PLUGIN_VAD=on` on
the stt side; ptt mode wants the `hotkey` plugin with a matching binding.
Always-on lifecycle comes free from the kernel's drop-in auto-spawn
(R10-01/R10-04) — no kernel change was ever needed (root `ROADMAP.md`).
