# mic plugin roadmap

Mirror of `sound` (speakers) on the capture side — the single owner of the
mic. Shipped v0.1.0: `mic_start` / `mic_stop` / `mic_status`, s16le PCM
over D-12 `AudioStreamChunk` streaming, record chain
`pw-cat --record` → `parec` → `arecord`.

## Non-goals

- **No mixing / parallel captures.** One session at a time, replace-on-start.
  Consumers needing concurrent taps should get a kernel-side tee, not N
  recorder processes fighting over the device.
- **No resampling / codec transcoding.** The plugin streams what the
  recorder emits (s16le at the requested rate/channels). Opus encoding
  stays with whoever needs it (precedent: `tts` encodes its own PCM).
- **No VAD, no echo cancellation, no gain control.** Signal processing is
  out of scope; the plugin is a dumb pipe from device to peer.
- **No file recording.** "Record to wav" composes as
  `mic_start → <peer that writes files>` or belongs in a future action if
  a real consumer appears.
- **No cloud speech.** Mic is transport-only; transcription lives in `stt`.

## Open options

- `format` beyond `pcm_s16le` (e.g. direct Opus via a piped encoder) — only
  when a consumer exists; the chunk proto already carries the codec field.
- A `plugin.mic.level` event (peak/RMS metering) for UIs — cheap to add on
  top of the framing loop, waiting for a real consumer.
