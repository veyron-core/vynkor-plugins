# stt plugin roadmap

Goal: give any Veyron plugin a way to transcribe audio — one blessed
path, provider quirks/auth/model handling in one place instead of every
plugin rolling its own client. Local-first: a fully offline engine is the
default, the cloud provider is an opt-in addition behind the same
interface.

## Decision: local in-process, cloud via `network`

Two halves, two different mechanics:

- **Cloud provider (`openai`)** does **not** open its own sockets and
  declares no `PERMISSION_NETWORK`. It calls the kernel-routed
  `http_request` action (owned by the `network` plugin) via
  `VeyronClient::send_action` — identical to `ai` and `tts`. SSRF
  blocklist, redirect handling, retry-backoff and response size caps in
  `network` apply for free; `stt`'s `plugin.json` has `"permissions": []`.
- **Local provider (`sherpa`)** opens no sockets at all. It links
  sherpa-onnx, loads an ONNX model from disk, and transcribes in-process.
  This is the deliberate shape of "local": no daemon, no subprocess, no
  network hop — the audio never leaves the machine. It's also why the
  plugin ships as a self-contained binary rather than shelling out to a
  CLI.

Two-tier design keeps the attack surface honest: cloud key material lives
behind the same `STT_PLUGIN_ALLOWED_KEY_ENVS` allowlist `ai`/`tts` use,
and the local model path is operator-set (`STT_PLUGIN_LOCAL_MODEL_DIR`),
never caller-controlled — a caller-supplied model path would be an
arbitrary file-read primitive.

## Naming

Plugin id: `stt`. Binary: `stt`. Mirrors `ai`/`network`/`tts` — short,
matches the "one blessed path per capability" convention. Actions:
`stt_transcribe`, `stt_models` (parallel to `tts_synthesize`,
`tts_voices`).

## v1 scope

- `stt_transcribe` action:

  Request (`ActionRequest.params_json`): `provider`, `audio_base64`
  (≤ 25 MiB decoded), `format` (`wav`|`pcm` for `sherpa`; `wav`|`mp3`|`ogg`
  for `openai`), `sample_rate_hz`/`num_channels` (raw pcm only),
  `language`, `prompt`, `temperature` (openai), `api_key_env` (openai,
  allowlisted), `base_url`/`model` overrides, `timeout_ms`.

  Response (`ActionResponse.data_json`): normalized
  `{ text, language, duration_seconds, model }`.

- `stt_models` action: the models a provider exposes
  (`sherpa`: the single operator model; `openai`: the known id list).

- Input formats: wav and raw 16-bit pcm are decoded in-process (strict
  RIFF parsing, downmix to mono); mp3/ogg pass through to `openai` as-is.

- Local model families: `transducer` (zipformer) and `whisper`.

## Known bugs (live-kernel audit 2026-08-22)

> **Fixed 2026-08 (`fix/live-audit-defects`, merged):** all blocking sherpa
> calls now run via `tokio::task::spawn_blocking` so ONNX init/load can't
> stall the async serve loop; isolated probes with real models return
> `models` instantly. See `docs/LIVE_KERNEL_AUDIT_2026-08-22.md` defect #2.

- **Local `sherpa` actions hang indefinitely before the model loads.**
  First full live-kernel audit (`docs/LIVE_KERNEL_AUDIT_2026-08-22.md`,
  defect #2): `stt_models`/`stt_transcribe` with provider `sherpa`
  (zipformer ru int8 on disk, `STT_PLUGIN_LOCAL_MODEL_TYPE=transducer`,
  path set via env) never respond — plugin sleeps, 0% CPU, RSS flat
  ~22 MB: sherpa init never starts. Kernel deadline → `ACTION_TIMEOUT`
  ~200 s. Same signature as `tts`'s local-engine hang in the same audit;
  both share the sherpa-onnx init path, so likely one root cause.
  Fix direction mirrors tts's: reproduce handler-direct with identical
  env, diff supervised vs bare spawn if needed, add a real-model
  integration test.

## Deliberately out of v1

- **Streaming/partial transcripts** — offline recognizers only. A future
  streaming variant would need a socket-based channel and is a different
  plugin shape. (2026-08-15: D-12 shipped `stt_listen_start`/`stt_listen_stop`
  — chunked PCM in, transcript out as an event — on top of the same local
  sherpa engine; still no *partial* transcripts, the recognizer runs once
  per stop.)
- **VAD / diarization / timestamps** — sherpa-onnx can emit word/speaker
  metadata; not exposed yet. The normalized result shape would need
  fields for segments.
- **More local model families** (`paraformer`, `sense_voice`, `moonshine`,
  ...) — each is a few lines in `sherpa.rs` once a real model is on disk;
  add when someone needs it.
- **More cloud providers** — the `Provider` trait
  (`build_http_request` + `parse_response`) is open for the next one.
