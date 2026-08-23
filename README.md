# veyron-plugins

Plugins for the [Vynkor](https://github.com/vynkor-core/vynkor) plugin
kernel.

## Naming: vynkor

Veyron is being renamed **vynkor** ("veyron core" contracted) — the kernel
and every sibling repo, eventually. **New code and docs in this repo use
`vynkor`**; keep "Veyron" only when referring to the historical name or a
rename in progress. The `vyn` binary name stays `vyn`. Stable identifiers —
`plugin_id` slugs, binary names, env-var names (`*_PLUGIN_*`), permission
strings — are protocol/config surfaces and keep their current spellings
even when prose says vynkor.

## Plugins

| Plugin | Path | Permissions | Description |
|---|---|---|---|
| `ping-pong` | `plugins/ping-pong-rs/` | none | Minimal example plugin that responds to ping actions. |
| `network` | `plugins/network/` | `PERMISSION_NETWORK` | Outbound HTTP for plugins/kernel via one `http_request` action. HTTP-only v1 (no WebSocket). See `plugins/network/README.md`. |
| `ai` | `plugins/ai/` | `PERMISSION_NETWORK`, `PERMISSION_SECRETS` | Provider-agnostic LLM chat completion (`chat_completion`) + embedding (`embedding`) for anthropic + openai-compatible providers (Ollama `nomic-embed-text` 768 via `network`). Routes through `network`'s gated `http_request`, so declares `network` itself (T-19). See `plugins/ai/README.md`. |
| `database` | `plugins/database/` | `PERMISSION_STORAGE` | Per-caller-namespaced KV + raw SQL storage over SQLite, five `db_*` actions. See `plugins/database/README.md`. |
| `tts` | `plugins/tts/` | `PERMISSION_NETWORK`, `PERMISSION_AUDIO_STREAM` | Text-to-speech via `tts_synthesize` + `tts_voices` + `tts_speak`: in-process local ONNX engine (sherpa: Kokoro/Piper, fully offline) + cloud providers (openai, elevenlabs) routed through `network`'s gated `http_request` (declares `network`, T-19); `tts_speak` streams Opus `AudioStreamChunk`s to a peer (D-12). See `plugins/tts/README.md`. |
| `stt` | `plugins/stt/` | `PERMISSION_NETWORK`, `PERMISSION_AUDIO_STREAM`, `PERMISSION_EVENT_PUBLISH` | Speech-to-text via `stt_transcribe` + `stt_models` + `stt_listen_start`/`stt_listen_stop`: in-process local ONNX engine (sherpa: zipformer/whisper, fully offline) + cloud provider (openai audio API) routed through `network`'s gated `http_request` (declares `network`, T-19); the listen actions stream PCM in and publish a `stt_text` event (D-12). See `plugins/stt/README.md`. |
| `secrets` | `plugins/secrets/` | `PERMISSION_SECRETS` | Encrypted credential/API-key vault (`secret_get`/`secret_set`/`secret_delete`/`secret_list`), ChaCha20-Poly1305 per-caller `.vault` files, master key via `SECRETS_PLUGIN_MASTER_KEY`. `ai`/`tts`/`stt` resolve provider keys vault-first with env-var fallback. See `plugins/secrets/README.md`. |
| `gated-write` | `plugins/gated-write/` | — | Reference impl of the D-09 confirmation gate: risky file write split into `request_write` (any caller, `requires_confirmation`) + `confirm_write` (allowlisted callers only), writes confined to a data dir. See `plugins/gated-write/README.md`. |
| `sync` | `plugins/sync/` | `PERMISSION_STORAGE`, `PERMISSION_EVENT_PUBLISH` | Host-side sync state primitive (D-13): versioned SQLite KV + `sync_get_snapshot`/`sync_get`/`sync_set`/`sync_del`, publishes `sync.delta` events on every mutation. |
| `sync-client` | `plugins/sync-client/` | `PERMISSION_SCHEDULER`, `PERMISSION_IPC_SEND` | Client-side mirror + heartbeat scheduler (D-13): subscribes to `sync.delta`, pulls `sync_get_snapshot` on (re)connect, pushes heartbeats via `sync_set` on a timer. |
| `notify` | `plugins/notify/` | `PERMISSION_NOTIFY` | Desktop/system notifications via host binaries — `notify-send` (libnotify), `wall`, `espeak`; argv-only spawn, never a shell. v0.2: `speak: true` озвучка через `tts`-плагин + `silent: true` inbox (`notify_list`/`notify_mark_read`/`notify_delete`). See `plugins/notify/README.md`. |
| `notes` | `plugins/notes/` | `PERMISSION_STORAGE`, `PERMISSION_EVENT_PUBLISH` | Note CRUD as a thin schema layer over `database` (`note:<id>` JSON docs, atomic id counter, tag filter/pagination), publishes `plugin.notes.changed`. Callers need no storage permission — `notes` holds it (T-19). See `plugins/notes/README.md`. |
| `calendar` | `plugins/calendar/` | `PERMISSION_STORAGE`, `PERMISSION_EVENT_PUBLISH`, `PERMISSION_NOTIFY` | Event CRUD + opt-in reminders (`remind_before_ms`): timer scan fires once at-most (`late` flag after downtime), publishes `plugin.calendar.changed`/`.due`, best-effort `notify_send`; rescheduling resets the fired flag. See `plugins/calendar/README.md`. |
| `media` | `plugins/media/` | — | Local MPRIS media playback control: 13 actions (`play/pause/play_pause/next/prev/stop/seek/seek_relative/volume/status/list_players/shuffle/loop`) over the session D-Bus via `zbus`; capability guards (`CanPlay`/`CanPause`/… → `ERR_MEDIA_NOT_SUPPORTED`), background `PropertiesChanged`/`Seeked` watcher feeding the position cache, unified `ERR_MEDIA_*` taxonomy. Fully offline (`permissions: []`). See `plugins/media/README.md`. |
| `clipboard` | `plugins/clipboard/` | `PERMISSION_CLIPBOARD` | Text clipboard read/write via host binaries — `wl-paste`/`wl-copy` (Wayland), `xclip`/`xsel` (X11); argv-only spawn, never a shell; size cap + per-spawn timeout. See `plugins/clipboard/README.md`. |

| `system` | `plugins/system/` | `PERMISSION_SYSTEM` | Local host queries + simple reversible controls: `sys_info`/`sys_battery`/`sys_procs`/`sys_volume[_set/_mute]`/`sys_brightness[_set]`/`sys_lock`/`sys_power_profile[_set]`. Backend detection with graceful `ERR_SYS_NOT_SUPPORTED`; Linux: UPower, wpctl→pactl, sysfs→brightnessctl (brightness clamps to non-blanking floor), ScreenSaver→loginctl, power-profiles-daemon; macOS: pmset/osascript/CGSession. Offline (`sandbox: false` for D-Bus/spawns). See `plugins/system/README.md`. |
| `scheduler` | `plugins/scheduler/` | `PERMISSION_STORAGE`, `PERMISSION_EVENT_PUBLISH` | Once/cron schedules over `database`: `schedule_set/get/list/delete` (`sched:<id>` JSON docs, atomic id counter). One-shots resolve `delay_ms` at set time and mark fired before dispatch (at-most-once; `late` flag on downtime catch-up); cron anchors to its last fire, missed occurrences skipped, fixed UTC offset only (`tz_offset_min`). Fires publish best-effort `plugin.scheduler.fired` or perform kernel-routed action calls (gated targets need operator-granted permissions — T-19, no laundering; failures land in `last_error`). See `plugins/scheduler/README.md`. |
| `sound` | `plugins/sound/` | `PERMISSION_AUDIO` | Audio output primitive — the single owner of the speakers: `sound_play` (file or inline base64, volume/device) spawns a host player argv-only and returns immediately; `sound_stop`/`sound_status` manage background clips. Chains: wav → `pw-cat --playback`/`paplay`/`aplay`, non-wav → `ffplay`; replace-on-play, idempotent stop, lazy reap. Fully offline. See `plugins/sound/README.md`. |
| `vector-db` | `plugins/vector-db/` | `PERMISSION_STORAGE`, `PERMISSION_EVENT_PUBLISH` | Embedding upsert/similarity search (`vec_upsert`/`vec_upsert_batch`/`vec_query`/`vec_get`/`vec_delete`/`vec_list`/`vec_stats`), per-caller SQLite, brute-force cosine, Ollama via `ai` (`nomic-embed-text` 768, `all-minilm:33m` 384). See `plugins/vector-db/README.md`, `USAGE.md`. |

Writing a new plugin? Start with [`docs/PLUGIN_AUTHORING.md`](docs/PLUGIN_AUTHORING.md) —
the single-reader loop / RPC-proxy pattern, kernel routing facts (T-19/T-04),
and the fake-kernel test harness. For how all shipped plugins behave on a
real secured kernel (2026-08-22 audit: results, resource profile, open
defects), see [`docs/LIVE_KERNEL_AUDIT_2026-08-22.md`](docs/LIVE_KERNEL_AUDIT_2026-08-22.md).

## Registry

`registry.json` is a slug-keyed v2 map: a root `meta` block (`apiVersion`,
`lastUpdated`) plus a root `revoked` list, then one entry per plugin slug. Each
slug entry carries `name`, `description`, `category`, `tags`, `status`, and
`source_url`, plus a `versions` map whose semver keys hold per-version delivery
metadata — an absolute `archive_url` into the `dist/<slug>/versions/<version>/`
hierarchy, `sha256`, `signature`, and the kernel compatibility range.
Permissions are not stored in the registry (execution metadata lives in each
plugin's manifest). The `dist/` tree is hierarchical: `dist/<slug>/latest.json`
(newest registered version), `dist/<slug>/assets/`, and
`dist/<slug>/versions/<version>/` (binary + source zips, a `plugin.json` browse
copy, `checksum.sha256`, `signature.sig`). A plugin only gets an entry once
it's packaged and released via `scripts/package.sh` — see each plugin's own
README for its current status.
