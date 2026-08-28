# notify plugin

Desktop/system notifications on the host, delivered by spawning external
binaries. Exposes five actions: `notify_send` (deliver a notification —
optionally silent, optionally spoken), `notify_providers` (list the
delivery backends), and the inbox actions `notify_list` / `notify_mark_read`
/ `notify_delete` (review stored notifications).

Three delivery providers, Linux only:

| Provider | Binary | What it is |
|---|---|---|
| `notify-send` | `notify-send` | libnotify desktop notification — the primary provider. On Arch: `sudo pacman -S libnotify`. |
| `wall` | `wall` | Broadcast a message to every logged-in terminal. Ships with util-linux, installed by default on most distros. |
| `espeak` | `espeak-ng` (falls back to `espeak`) | Spoken alert. Optional: `sudo pacman -S espeak-ng`. |

Every delivery spawns the binary directly with argv — never a shell — so
message/title content cannot inject commands.

## Operator note

`notify` declares one kernel permission — `notify` (`plugin.json`:
`"permissions": ["notify"]`, and the register manifest carries
`PERMISSION_NOTIFY`). All five actions are gated with a per-action
`"permission": "notify"` (Manifest v2), so the kernel's anti-laundering
check requires every caller to hold the `notify` permission too.

It opens no sockets of its own — each delivery spawns a host binary with
argv only. Run it with `sandbox: true`; if the kernel's sandbox blocks
spawning host binaries (or `notify-send` can't reach the session DBus
notification daemon from inside it), set `sandbox: false` for this plugin
— see `config.example.yaml`. Notification content stays argv-only either
way.

## Action: `notify_send`

Request (`ActionRequest.params_json`):

```json
{
  "provider": "notify-send",
  "title": "Build finished",
  "message": "cargo build succeeded in 41s",
  "urgency": "normal",
  "timeout_ms": 5000,
  "app_name": "ci",
  "speak": true
}
```

- `provider` — `"notify-send"` | `"wall"` | `"espeak"`. Optional, default
  `"notify-send"`.
- `title` — optional, capped at 256 bytes. Omitted from the spawned argv
  when empty.
- `message` — **required**, non-empty, capped at 4096 bytes.
- `urgency` — optional, `low` | `normal` | `critical`. Only sent to
  `notify-send`.
- `timeout_ms` — optional, `1..=600000` ms (`0` leaves the provider
  default). Only sent to `notify-send`.
- `app_name` — optional notify-send app name; defaults to
  `NOTIFY_PLUGIN_APP_NAME`, then `vynkor`.
- `silent` — optional, default `false`. Store only, no delivery — see
  "Silent notifications & the inbox".
- `speak` — optional, default `false`. Also synthesize the text through
  the `tts` plugin and play the audio — see "Spoken notifications".

The `wall` and `espeak` providers take the same shape minus the
notify-send-only fields — `urgency`/`timeout_ms`/`app_name` are ignored by
them. A `wall` example:

```json
{ "provider": "wall", "title": "Deploy", "message": "production release rolling out" }
```

`wall` gets a single text argument (`"title: message"` when a title is set,
else just the message), broadcast to every logged-in terminal. `espeak`
speaks the same text.

Response (`ActionResponse.data_json`) on success:

```json
{
  "id": "1700000000000-1",
  "delivered": true,
  "provider": "notify-send",
  "command": "notify-send",
  "detail": "",
  "spoken": true,
  "speak_error": ""
}
```

- `id` — inbox audit-entry id (empty when the inbox is unavailable).
- `delivered` — always `true` on this path.
- `provider` — canonical provider id the message went through.
- `command` — the binary that was invoked (`espeak-ng` or `espeak` for the
  espeak provider, whichever is installed).
- `detail` — trimmed stdout of the delivery binary (usually empty).
- `spoken` — `true` when the tts озвучка succeeded.
- `speak_error` — озвучка failure message; empty when `spoken` is `true`
  or `speak` was not requested.

Errors → `ACTION_ERROR` with a human-readable message: malformed/missing
request fields, an unknown provider, a provider disabled by
`NOTIFY_PLUGIN_ENABLED_PROVIDERS`, a missing binary on `PATH` (naming the
binary to install), or a non-zero exit from the delivery binary (with its
stderr).

## Silent notifications & the inbox

`silent: true` stores the notification **without delivering it**:

```json
{ "silent": true, "title": "Review", "message": "3 PRs need attention" }
```

```json
{ "id": "1700000000000-7", "stored": true, "silent": true, "delivered": false }
```

`provider` and `speak` are ignored on the silent path — it is store-only.
Silent notifications are the mechanism a future agent plugin uses to
observe activity without interrupting the user: record it now, and let the
agent (or the user) review the inbox later.

Review via the inbox actions, all gated by `"permission": "notify"`:

```json
// notify_list  →  { "notifications": [ ... ] }  (newest first; read entries hidden unless include_read)
{ "include_read": false }

// notify_mark_read  →  { "updated": true }   (true only when the entry existed and was unread)
{ "id": "1700000000000-7" }

// notify_delete  →  { "deleted": true }      (true only when the entry existed)
{ "id": "1700000000000-7" }
```

Each stored entry: `{id, created_at_ms, title, message, provider,
delivered, silent, spoken, read}`. Every delivered notification also
records an audit entry (`delivered: true`), so the inbox is a full history,
not just silent messages. The inbox keeps the newest 500 entries.

The inbox needs `NOTIFY_PLUGIN_DATA_DIR` (`$NOTIFY_PLUGIN_DATA_DIR/inbox.json`,
atomic writes, mode 0600). Silent notifications and the three inbox actions
fail with `ACTION_ERROR` when it is unset; **normal deliveries never
require it** — they just skip the store (logged, not fatal).

## Spoken notifications (`speak: true`)

With `"speak": true`, after a successful delivery the notification text is
synthesized through the `tts` plugin's `tts_synthesize` action and played
on the host. The `tts` plugin must be registered and running with its own
provider/model config (see `plugins/tts/`); `tts_synthesize` carries no
per-action permission, so `notify` needs no extra permission to call it.

Operator knobs (env): `NOTIFY_PLUGIN_TTS_PROVIDER` (default `sherpa`),
`NOTIFY_PLUGIN_TTS_VOICE` (optional; the `voice` key is omitted when
unset), `NOTIFY_PLUGIN_TTS_FORMAT` (default `wav`; `pcm` is rejected as
not directly playable), `NOTIFY_PLUGIN_AUDIO_PLAYER` (player binary
override; otherwise `wav` auto-detects `paplay` / `pw-play` / `aplay`, and
any non-wav format needs `ffplay` — see `config.example.yaml` for Arch
package names).

**Speak is best-effort.** A failed озвучка logs `[notify] speak failed: ...`
to stderr, returns `"spoken": false` plus `"speak_error"`, and the action
still succeeds — the notification was delivered.

## Action: `push_send` — ntfy/Gotify to the phone (v0.3)

Routes through the `network` plugin's gated `http_request` (same T-19 caller
model as `ai`/`search`: `notify` holds `PERMISSION_NETWORK`, opens no sockets
itself, `requires: ["network"]`). JSON publishing APIs only, so UTF-8 text
needs no header encoding; tokens ride in headers (`Authorization: Bearer` for
ntfy, `X-Gotify-Token` for gotify), never in URLs or logs.

```json
{ "provider": "ntfy", "topic": "vyn", "title": "Deploy",
  "message": "production release rolling out", "priority": 4,
  "tags": ["white_check_mark"] }
→ { "pushed": true, "provider": "ntfy", "server": "ntfy.sh", "status": 200 }
```

- `server` must be on the operator allowlist `NOTIFY_PLUGIN_PUSH_SERVERS`
  (comma-separated hosts, default `ntfy.sh`; add your self-hosted Gotify
  host there). A caller-chosen host would make notify an exfiltration
  channel.
- Optional tokens from env: `NOTIFY_PLUGIN_NTFY_TOKEN`,
  `NOTIFY_PLUGIN_GOTIFY_TOKEN`. Absent → anonymous publish.
- `topic` is required for ntfy (`[A-Za-z0-9_-]{1,64}`), rejected for gotify.
- Errors → `ACTION_ERROR`: allowlist violation, provider HTTP non-2xx
  (body snippet included), network failure.

## Action: `notify_providers`

```json
{}
```

Returns all three providers with their availability:

```json
[
  { "id": "notify-send", "name": "notify-send", "available": true, "description": "libnotify desktop notification via notify-send" },
  { "id": "wall", "name": "wall", "available": true, "description": "broadcast a message to all logged-in terminals (wall)" },
  { "id": "espeak", "name": "espeak", "available": false, "description": "spoken alert via espeak-ng (falls back to espeak)" }
]
```

`available` is true when the provider is enabled by operator policy AND
its binary is installed (`espeak`: `espeak-ng` **or** `espeak` present).

## Configuration

`notify` reads no config file itself — everything is environment variables
set in the kernel's `config.yaml`, under this plugin's `env:` list — see
`config.example.yaml` in this directory.

- `NOTIFY_PLUGIN_APP_NAME` — optional, default `vynkor`. Default app name
  for `notify-send` when a request omits `app_name`.
- `NOTIFY_PLUGIN_ENABLED_PROVIDERS` — optional, comma-separated list of
  enabled providers (`notify-send,wall,espeak`). Empty/unset = all enabled.
  A provider not on the list is rejected at call time and reported
  unavailable by `notify_providers`; an unknown id in the list makes every
  delivery fail.
- `NOTIFY_PLUGIN_DATA_DIR` — **required only for the inbox features**
  (`silent: true`, `notify_list` / `notify_mark_read` / `notify_delete`).
- `NOTIFY_PLUGIN_TTS_PROVIDER` / `NOTIFY_PLUGIN_TTS_VOICE` /
  `NOTIFY_PLUGIN_TTS_FORMAT` — tts synthesis parameters for
  `speak: true` (defaults: `sherpa` / unset / `wav`).
- `NOTIFY_PLUGIN_AUDIO_PLAYER` — optional player binary override for
  `speak: true`; default auto-detect (`paplay`/`pw-play`/`aplay` for wav,
  `ffplay` otherwise).

## Security

- **argv-only spawn, never a shell.** Every delivery and the audio player
  are `Command::new(binary).args(...)` with the message/title/file passed
  as individual argv elements. There is no `sh -c` anywhere, so
  notification content cannot inject commands, no matter what a caller
  sends.
- **Size caps.** `message` ≤ 4096 bytes, `title` ≤ 256 bytes, inbox ids ≤
  128 bytes — bounds the argv and any terminal/desktop rendering cost, and
  bounds the kernel's copy of the request.
- **Permission gate.** All actions carry `"permission": "notify"` (Manifest
  v2), and the plugin itself registers with `PERMISSION_NOTIFY` — a plugin
  without the `notify` permission cannot call any of them (the kernel's
  anti-laundering check).
- **`wall` is OS-gated.** Writing to other users' terminals requires
  membership in the `tty` group (or root) — an OS-level gate the plugin
  cannot override; unauthorized `wall` attempts fail with the binary's own
  error, surfaced in the `ACTION_ERROR` message.
- **Content is loud by design.** `wall`, `espeak`, and `speak: true` deliver
  whatever text a caller sends to *every* logged-in terminal / the host
  speaker; callers should treat `notify_send` as
  unverified-input-to-the-user, like any notification API.
- **The inbox is plaintext.** Silent notifications are stored as plaintext
  JSON on disk under `NOTIFY_PLUGIN_DATA_DIR` (atomic writes, mode 0600) —
  the inbox is not encrypted; sensitive messages should not be sent
  silent.
- **Audio content is never logged.** tts responses and decoded audio are
  never written to logs; the temp audio file is removed after playback.

## Testing

`cargo test` — unit tests cover request parsing/validation (caps, urgency,
timeout range, silent/speak defaults, inbox ids), provider-id parsing,
`notify_send_args` ordering and content, `binary_in_path`, the provider
list, the `NOTIFY_PLUGIN_ENABLED_PROVIDERS` parser, the tts request
builder, the inbox store (roundtrip, read filtering, mark/delete across
reopen, 500-entry pruning, corrupt-file loudness), and the response
shapes. No live binaries or kernel are needed (tests only probe `PATH`
and use tempdirs). End-to-end delivery was verified against a real
kernel + `notify` with `libnotify` installed; there is no automated
integration test for that yet.
