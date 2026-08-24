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
| `daemon_status` | `{}` | `{enabled, busy, turns_completed, last_turn}` |
| `daemon_turn` | `{text?}` | run one voice cycle now; `text` skips mic/stt. See below |
| `daemon_say` | `{text}` | synthesize + play text through tts + sound |
| `daemon_ask` | `{prompt}` | agent round-trip; the answer is spoken aloud and returned |

### The voice turn

One cycle of [`run_voice_turn`](src/lib.rs):

1. `stt_listen_start` opens an accumulation buffer on `stream_id`.
2. `mic_start {target: "stt", stream_id}` points the mic at it.
3. The capture window elapses (`DAEMON_PLUGIN_TURN_MS`, default 6 s).
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
malformed request.

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
  both manual `daemon_turn` and the background loop.
- `plugin.daemon.state.changed` — `{"enabled": true|false}` on enable/disable.

## Concurrency architecture

Single-reader select loop + channel-fronted RPC proxy + spawned tasks —
the calendar/sync-client pattern (`docs/PLUGIN_AUTHORING.md` §1). The serve
loop exclusively owns the `VynkorClient`; handler tasks and the timer-driven
turn task reach mic/stt/agent/tts/sound through the `Rpc` proxy channel, so
a turn started by a tick can never eat an inbound user request
(`send_action`'s discard-while-waiting would). Loop state (enabled/busy/
counter/last turn) lives behind atomics + small mutex slots in
[`DaemonState`](src/lib.rs) — no `.await` while holding a guard.

## Testing

`cargo test` — 9 unit tests over request parsing/validation, plus 14 e2e
tests driving the real serve loop against a fake kernel over
`UnixStream::pair` (registration handshake asserting the declared
permissions, scripted mic/stt/agent/tts/sound stand-ins recording every
outbound call in arrival order): full-pipeline order, text-bypass turn,
silent turn, per-stage failure routing (payload vs ACTION_ERROR), busy-slot
rejection, and enable/disable loop ticks. No live kernel, no audio hardware.

## Status

v0.1. Depends on `mic`, `stt`, `agent`, `tts` and `sound` being registered;
the daemon degrades per-stage (see `USAGE.md` errors) but needs all five for
a full answered turn. Always-on lifecycle comes free from the kernel's
drop-in auto-spawn (R10-01/R10-04) — no kernel change was ever needed (root
`ROADMAP.md`).
