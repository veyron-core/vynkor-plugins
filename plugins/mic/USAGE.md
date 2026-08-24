# mic plugin — caller's guide

Capture microphone audio from any plugin or client. Three actions:
`mic_start` spawns a host recorder and streams raw PCM to a peer you name,
`mic_stop` ends it, `mic_status` reports what is capturing. The model:

- **Control is on-demand** — nothing records until someone calls
  `mic_start`, and capture stops at `mic_stop`, recorder death, or plugin
  shutdown.
- **Audio is pushed, not polled** — PCM flows to the `target` peer as
  `AudioStreamChunk` envelopes while the session lives; `mic_start`
  itself returns immediately.
- **Single owner** — one session at a time; a new `mic_start` replaces
  whatever was capturing (`replaced: true`).

## `mic_start`

### Request

`ActionRequest.params_json`:

```json
{
  "target": "stt",
  "format": "pcm_s16le",
  "device": null,
  "sample_rate_hz": 16000,
  "num_channels": 1,
  "chunk_ms": 100,
  "stream_id": 42
}
```

Required:

- `target` — the peer slug that receives the captured PCM (e.g. `"stt"`,
  a WS client slug like `daemon`, a remote device slug). Must be on the
  operator's `MIC_PLUGIN_IPC_TARGETS` allowlist (see
  [Gating](#gating-what-you-and-the-operator-must-set-up)).

Optional:

- `format` — only `pcm_s16le` in v0.1 (default). That is the D-12 codec
  every consumer already decodes.
- `device` — capture device/source name. Applied by pw-cat (`--target`),
  parec (`--device`), arecord (`-D`). Falls back to the operator's
  `MIC_PLUGIN_DEVICE`; unset = the backend's default source.
- `sample_rate_hz` — `8000..=192000` (default `16000`, or
  `MIC_PLUGIN_RATE`).
- `num_channels` — `1..=8` (default `1` = mono, or `MIC_PLUGIN_CHANNELS`).
- `chunk_ms` — target duration of one streamed chunk, `10..=1000`
  (default `100`, or `MIC_PLUGIN_CHUNK_MS`). At 16 kHz mono s16le that is
  ~3200 bytes per chunk.
- `stream_id` — pin the `AudioStreamChunk.stream_id` (auto-allocated when
  omitted). The receiver demultiplexes concurrent streams by this id;
  pass your own when coordinating with `stt_listen_start`.

### Response

`ActionResponse.data_json` on success — sent as soon as the recorder
process exists, while capture continues in the background:

```json
{
  "ok": true,
  "session_id": "session-3",
  "stream_id": 42,
  "target": "stt",
  "recorder": "pw-cat",
  "format": "pcm_s16le",
  "sample_rate_hz": 16000,
  "num_channels": 1,
  "chunk_ms": 100,
  "replaced": false
}
```

- `session_id` — handle for `mic_stop`.
- `recorder` — which backend won the chain: `pw-cat` → `parec` → `arecord`
  (first installed wins; `MIC_PLUGIN_RECORDER` pins one).
- `stream_id` — echoed in every chunk of this session.
- `replaced` — `true` when a previous session was stopped to start this
  one.

## What the receiver gets

While the session lives, `target` receives `AudioStreamChunk` envelopes
routed by the kernel:

```jsonc
{
  "stream_id": 42,
  "codec": "PCM_S16LE",     // raw 16-bit signed little-endian
  "sample_rate": 16000,
  "channels": 1,
  "data": "<bytes>",        // whole samples; fixed-size until the last chunk
  "end_of_stream": false    // true on exactly one final chunk
}
```

The final chunk carries any sub-frame remainder (trimmed to whole samples,
possibly empty) with `end_of_stream: true`. Every termination path —
`mic_stop`, recorder death, plugin shutdown — ends with that marker, so a
receiver never waits on a stream whose owner vanished.

## `mic_stop`

```json
{ "session_id": "session-3" }
```

Omit `session_id` to stop everything. Idempotent — unknown or
already-finished ids yield an empty list:

```json
{ "stopped": ["session-3"] }
```

The recorder process is killed immediately; the final `end_of_stream`
chunk is flushed to the peer after this response is already on its way
back to you.

## `mic_status`

```json
{}
```

```json
{
  "capturing": [
    {
      "id": "session-3",
      "stream_id": 42,
      "target": "stt",
      "recorder": "pw-cat",
      "device": null,
      "format": "pcm_s16le",
      "sample_rate_hz": 16000,
      "num_channels": 1,
      "chunk_ms": 100,
      "chunks_sent": 17
    }
  ],
  "count": 1
}
```

Sessions whose recorder died disappear here lazily on the next action
(no watcher task) — call `mic_status` to converge the view.

## Gating (what you and the operator must set up)

Two independent gates, both satisfied before audio flows:

1. **Your JWT needs `PERMISSION_AUDIO`** — the kernel gates every `mic_*`
   action on it. Without it: `ERR_PERMISSION_DENIED` from the kernel.
2. **The target must be allowlisted** — the operator lists every slug mic
   may unicast to in the plugin env
   (`MIC_PLUGIN_IPC_TARGETS="stt,daemon,device.phone.stt"`). Default-deny:
   unset means no target works. A non-listed target is refused by the
   kernel's T-04 unicast gate — your `mic_start` succeeds, but no chunk
   ever reaches the peer; fix the env, not the code.

## Errors

Any failure returns `ACTION_ERROR` with a human-readable `error` string.
Callers can hit these:

| Error | When |
|---|---|
| `invalid params_json: ...` | params aren't valid JSON |
| `ERR_MIC_BAD_PARAMS: params must be a JSON object` | params aren't an object |
| `ERR_MIC_BAD_PARAMS: 'target' is required ...` | no `target` |
| `ERR_MIC_BAD_PARAMS: 'target' must be a string` / `'target' must be non-empty` | bad `target` |
| `ERR_MIC_BAD_PARAMS: unsupported 'format' 'X' (only "pcm_s16le" in v0.1)` | anything but pcm_s16le |
| `ERR_MIC_BAD_PARAMS: '<param>' must be within [min, max], got N` | out-of-range `sample_rate_hz` `[8000, 192000]`, `num_channels` `[1, 8]`, `chunk_ms` `[10, 1000]`, or `stream_id` `[1, 4294967295]` |
| `ERR_MIC_BAD_PARAMS: 'device' must be non-empty` | empty `device` |
| `ERR_MIC_BAD_PARAMS: MIC_PLUGIN_RECORDER='X' is not a known recorder (expected one of: pw-cat, parec, arecord)` | bad operator pin |
| `ERR_MIC_PROVIDER_MISSING: binary 'X' not found on PATH` | one candidate missing — the chain falls through automatically |
| `ERR_MIC_PROVIDER_MISSING: no working audio recorder found (tried: ...); install pw-cat ..., parec ..., arecord ..., or pin MIC_PLUGIN_RECORDER` | nothing installed |
| `ERR_MIC_SPAWN_FAILED: spawn 'X' failed: ...` | binary exists but couldn't spawn |
| `ERR_MIC_SPAWN_FAILED: recorder produced no stdout pipe` | broken spawn contract |

## Common patterns

### Voice → text via `stt` (the D-12 loop)

```jsonc
// 1. open the accumulation buffer on stt
stt_listen_start { "stream_id": 42, "sample_rate_hz": 16000 }

// 2. capture into it — same stream_id, same rate
mic_start { "target": "stt", "stream_id": 42, "sample_rate_hz": 16000 }

// ... user speaks; PCM accumulates in stt ...

// 3. end of utterance
mic_stop {}                                  // flushes end_of_stream to stt
stt_listen_stop { "stream_id": 42 }          // → transcript + stt_text event
```

Local only by construction: the audio never leaves the machine — the
recorder reads the device, `stt` transcribes in-process, and only text is
published.

### Stream to a connected client (webclient / daemon / phone)

Clients register under their own slug over the kernel WS gateway; point
`target` at them and demux by `stream_id` client-side:

```json
{ "target": "daemon", "sample_rate_hz": 16000, "chunk_ms": 50 }
```

Smaller `chunk_ms` lowers latency at the cost of more envelopes; 50 ms is
a reasonable interactive floor, 100 ms the comfortable default.

### Replace instead of coordinate

You don't need to stop a session before starting another — `mic_start`
does it atomically and tells you via `replaced: true`:

```jsonc
let r = mic_start({ "target": "stt", ... });
if r.replaced { /* you took the mic from whoever had it */ }
```

### Consuming in your own plugin

Handle the inbound payload in your serve loop — see
`plugins/stt/src/main.rs` for the reference implementation: match
`envelope::Payload::AudioStreamChunk`, key buffers by `stream_id`, treat
`end_of_stream == true` as the flush signal, and reject rate mismatches
loudly (a mismatched chunk means a spliced stream).

### One mic, many consumers?

Not supported — and deliberate. Mic is the *single owner* of the device
(mirror of `sound` owning speakers). If two consumers need the same
audio, start one session toward a tee/mixer peer rather than fighting
over sessions.
