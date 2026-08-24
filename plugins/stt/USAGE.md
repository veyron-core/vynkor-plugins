# stt plugin — caller's guide

Transcribe audio to text from any plugin. Two actions, two providers:
`sherpa` (local, in-process, fully offline) and `openai` (cloud, routed
through the `network` plugin). All results come back in one normalized
shape regardless of provider.

## `stt_transcribe`

### Request

`ActionRequest.params_json`:

```json
{
  "provider": "openai",
  "audio_base64": "<base64 of the audio bytes>",
  "format": "wav",
  "language": "en",
  "prompt": "The transcript is about weather.",
  "temperature": 0.0,
  "api_key_env": "OPENAI_API_KEY",
  "model": "whisper-1",
  "timeout_ms": 30000
}
```

Required:

- `provider` — `"sherpa"` | `"openai"`.
- `audio_base64` — the audio bytes, base64-encoded. Decoded size ≤ 25 MiB.

Optional:

- `format` — `sherpa`: `wav` (default) | `pcm`; `openai`: `wav` (default)
  | `mp3` | `ogg`. Raw `pcm` (16-bit little-endian) needs `sample_rate_hz`
  and `num_channels`; container formats carry their own.
- `language` — ISO-639-1 hint, e.g. `"de"`. Letters only, normalized to
  lowercase. Echoed back in the response.
- `prompt` — Whisper-style context hint; `openai` only, ≤ 1000 chars.
- `temperature` — `0.0`..=`1.0`; `openai` only, default unset (provider
  default).
- `sample_rate_hz`, `num_channels` — required with `format: "pcm"` and
  `provider: "sherpa"`.
- `api_key_env` — the lookup handle for the key, `openai` only. Never a
  literal key. Must be on the operator's
  `STT_PLUGIN_ALLOWED_KEY_ENVS` allowlist. Resolved secrets-first: `stt`
  reads the key from its own `secrets` vault under this exact name
  (`secret_get`), falling back to the same-named env var of the `stt`
  process — the vault wins when both exist.
- `base_url` — API base override; `openai` only. Default
  `https://api.openai.com/v1`.
- `model` — `openai`: one of `whisper-1`, `gpt-4o-transcribe`,
  `gpt-4o-mini-transcribe` (default `whisper-1`); `sherpa`: ignored (the
  operator-configured model is used).
- `timeout_ms` — default `30000`, capped at `60000`. Cloud requests are
  further capped at `network`'s 30 s HTTP limit.

### Response

`ActionResponse.data_json` on success:

```json
{
  "text": "Hello from Veyron.",
  "language": "en",
  "duration_seconds": 2.4,
  "model": "whisper-1"
}
```

- `text` — the transcript, trimmed.
- `language` — your `language` hint for `openai`; the model's language for
  `sherpa` (your hint wins if you sent one). `""` when unknown.
- `duration_seconds` — derived from the wav header for `wav` uploads,
  `0` otherwise (mp3/ogg).
- `model` — `openai`: the resolved model id; `sherpa`: `sherpa:<family>`
  (e.g. `sherpa:transducer`).

### Per-provider notes

- **`sherpa`** — audio never leaves the machine. `wav` or raw `pcm` only.
  The first request loads the ONNX model (can take a few seconds); later
  requests use the cached engine. A caller-supplied `language` is applied
  per-request when the model family supports it (whisper).
- **`openai`** — the upload goes through `network`'s `http_request`
  action as a multipart body. Needs `network` registered + running, an
  allowlisted `api_key_env`, and the key resolved from `stt`'s `secrets`
  vault under that name (with the same-named env var as fallback — vault
  wins).

### Examples

Minimal local (wav upload):

```json
{ "provider": "sherpa", "audio_base64": "UklGRg..." }
```

Local raw pcm (16 kHz mono):

```json
{
  "provider": "sherpa",
  "audio_base64": "AP8A/wD/AP8A/wA=",
  "format": "pcm",
  "sample_rate_hz": 16000,
  "num_channels": 1
}
```

Cloud, German audio, mp3:

```json
{
  "provider": "openai",
  "audio_base64": "SUQzBAAAAA...",
  "format": "mp3",
  "language": "de",
  "api_key_env": "OPENAI_API_KEY"
}
```

## `stt_models`

```json
{ "provider": "openai" }
```

Returns the models the provider exposes as a list of `{ "id", "name" }`
objects. Use an `id` as the `model` value in `stt_transcribe` (openai).

- `sherpa` — `[{ "id": "sherpa:transducer", ... }]` (the one
  operator-configured model).
- `openai` — the known ids: `whisper-1`, `gpt-4o-transcribe`,
  `gpt-4o-mini-transcribe`.

## `stt_listen_start` / `stt_listen_stop`

D-12 voice pipeline: the client-STT → host-text leg. A mic-capable peer
(e.g. the Android device agent's mic capability) streams PCM to `stt` as
`AudioStreamChunk` envelopes (codec `PCM_S16LE`), and `stt` transcribes
the accumulated audio locally and publishes the text as an event. The
audio never leaves the device — only the transcript does.

```json
{ "stream_id": 1, "sample_rate_hz": 16000, "num_channels": 1 }
```

- `stt_listen_start` — open an accumulation buffer for a stream. `stream_id`
  (default `1`) must match the inbound chunks' `stream_id`; `sample_rate_hz`
  is required and is locked for the stream's lifetime (a mismatched chunk
  rate is rejected); `num_channels` (default `1`) is downmixed to mono;
  `language` is an optional ISO-639-1 hint applied at transcription time.
  Response: `{ "stream_id": 1, "status": "listening" }`.

The mic peer then sends one `AudioStreamChunk` envelope per chunk with
`codec: PCM_S16LE`, the stream's `sample_rate_hz`/`num_channels`, and the
16-bit little-endian PCM in `data`. Chunks accumulate until:

```json
{ "stream_id": 1 }
```

- `stt_listen_stop` — transcribe the buffered audio with the local sherpa
  model, publish the result as an event (namespaced
  `plugin.stt.stt_text`), and return it in the response:

```json
{
  "stream_id": 1,
  "text": "hello from the phone",
  "language": "en",
  "duration_seconds": 1.8,
  "model": "sherpa:transducer"
}
```

Subscribers receive the same object as the `stt_text` event payload. A
stop with no buffered audio errors (`listen stream N has no audio
buffered`). Requires `PERMISSION_AUDIO_STREAM` (to receive the chunks) and
`PERMISSION_EVENT_PUBLISH` (to publish the transcript). Local (`sherpa`)
only — the listen path has no cloud provider.

### Voice-activity events (opt-in, `STT_PLUGIN_VAD=on`)

While a stream accumulates, an energy VAD publishes boundaries as
best-effort events:

| Namespaced event | Payload | Meaning |
|---|---|---|
| `plugin.stt.stt_speech_started` | `{"stream_id": 1}` | two consecutive above-threshold chunks opened an utterance |
| `plugin.stt.stt_speech_ended` | `{"stream_id": 1, "speech_ms": 900}` | `STT_PLUGIN_VAD_SILENCE_MS` of quiet closed it after ≥ `STT_PLUGIN_VAD_MIN_SPEECH_MS` of speech |

Transcription is unchanged — `stt_listen_stop` always transcribes the
whole buffer regardless of VAD state; the events exist so orchestrators
(the daemon's vad mode) can end turns when the user actually stops talking.
Knobs: `STT_PLUGIN_VAD` (`on`), `STT_PLUGIN_VAD_RMS_THRESHOLD` (500),
`STT_PLUGIN_VAD_SILENCE_MS` (1200), `STT_PLUGIN_VAD_MIN_SPEECH_MS` (240).
Too-short blips reset quietly without an ending event.

## Errors

Any failure returns `ACTION_ERROR` with a human-readable `error` string.
Callers can hit these:

| Error | When |
|---|---|
| `invalid JSON: ...` | `params_json` isn't valid JSON |
| `missing required field: provider` | no `provider` |
| `unsupported provider: X` | `provider` isn't `sherpa`/`openai` |
| `missing required field: audio_base64` | no audio |
| `audio_base64 must not be empty` | empty string |
| `audio_base64 is not valid base64: ...` | malformed base64 |
| `audio_base64 exceeds max size of 26214400 bytes` | over the cap |
| `audio_base64 decoded to an empty audio payload` | base64 of nothing |
| `sherpa supports formats wav\|pcm, got: X` | bad format for `sherpa` |
| `openai supports formats wav\|mp3\|ogg, got: X` | bad format for `openai` |
| `openai supports formats wav\|mp3\|ogg (raw pcm is not accepted)` | `pcm` + `openai` |
| `language 'X' is not a valid ISO-639-1 code (letters only)` | bad language |
| `prompt exceeds max length of 1000 chars` | prompt too long |
| `missing required field: api_key_env` | `openai` without a key env name |
| `api_key_env must not be empty` | empty key env name |
| `unknown openai model 'X' (known: ...)` | bad `model` for `openai` |
| `api_key_env 'X' is not in the operator's STT_PLUGIN_ALLOWED_KEY_ENVS allowlist` | un-allowlisted key env name |
| `key 'X' is neither in the secrets vault nor set as an environment variable` | allowlisted but unresolvable key handle (not in the vault, env var unset) |
| `pcm input requires num_channels` / `pcm input requires sample_rate_hz` | `sherpa` + `pcm` without metadata |
| `sherpa accepts wav\|pcm input only` | `sherpa` with mp3/ogg audio |
| `not a RIFF/WAVE file`, `wav missing fmt chunk`, `unsupported wav encoding: X`, `unsupported wav bit depth: X`, `wav missing data chunk` | broken wav upload |
| `STT_PLUGIN_LOCAL_MODEL_DIR is not set ...` / `is not a directory` | operator config problem |
| `STT_PLUGIN_LOCAL_MODEL_TYPE is not set ...` / `is unsupported ...` | operator config problem |
| `missing required model file: <path>` | model dir incomplete |
| `sherpa-onnx failed to load model from ...` | model files invalid for the family |
| `network plugin call failed: ...` | `network` unreachable/erroring |
| `network plugin error: ...` | `network` returned non-OK |
| `malformed network response: ...` | `network` reply isn't the expected shape |
| `provider returned HTTP X: <body>` | non-2xx from the provider (e.g. 401 bad key, 400 bad audio) |
| `malformed openai transcription response: ...` | unexpected body on 2xx |
| `openai returned an empty transcript` | provider returned blank text |
| `missing required field: sample_rate_hz` / `sample_rate_hz must be > 0` | `stt_listen_start` without a rate |
| `num_channels must be > 0` | `stt_listen_start` with bad channel count |
| `listen stream N is already active` | `stt_listen_start` on a live stream |
| `no active listen stream N` | chunk/stop for an unknown or finished stream |
| `stream N rate mismatch: negotiated X Hz, chunk at Y Hz` | chunk rate differs from start |
| `stream N chunk has odd byte length ...` | non-16-bit-aligned PCM chunk |
| `listen stream N has no audio buffered` | `stt_listen_stop` on an empty stream |
| `failed to publish stt_text event: ...` | event bus rejected the publish |

Never logged, never echoed: the resolved API key. Errors only ever
reference the env var *name* (`api_key_env`).

## Common patterns

- **Record once, transcribe many**: plugins capture audio (16 kHz mono
  wav is the sweet spot — small uploads, `duration_seconds` works), then
  call `stt_transcribe` per clip.
- **Language hints**: pass `language` when you know it — Whisper-family
  decoders use it to constrain decoding, and the response echoes it back
  for your own bookkeeping.
- **Context prompts**: for tricky domains (numbers, names, jargon), pass
  `prompt` with the `openai` provider; Whisper uses it as decoding context.
- **Local = private**: use `sherpa` for anything sensitive — the audio
  and model run entirely in-process.
- **Stream mic audio with `stt_listen_*`**: for a device-agent pipeline
  (D-12/D-14), stream `PCM_S16LE` chunks rather than base64-uploading
  clips — the audio never leaves the device, and the transcript reaches
  the host as the `stt_text` event. Subscribe to `plugin.stt.stt_text`
  on the host to see device speech.
- **One `stream_id` per mic**: concurrent streams are supported, but each
  must be `stt_listen_start`ed before chunks arrive; a chunk for an
  unknown stream is dropped with a log line.
