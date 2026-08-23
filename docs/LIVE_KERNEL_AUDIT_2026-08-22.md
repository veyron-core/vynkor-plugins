# Live-kernel plugin audit — 2026-08-22

First full audit of every shipped plugin against a **real kernel**: release
builds, secured (JWT + frame-MAC), local Hyprland/Wayland desktop session.
18/18 plugins registered; functional matrix, resource profile, defects.

> Rig snapshot: `scripts/live-audit/` (WS client harness + test matrices).
> It was run against scratch configs under `/tmp`; see that directory's
> header comments before reusing.

## Setup

| Piece | Value |
|---|---|
| Kernel | `vyn` 0.1.0, release build, tmux foreground |
| Auth | JWT (HS256) + per-frame HMAC-SHA256 (HKDF session keys) active |
| API | `https://127.0.0.1:8130` (TLS is default; plain HTTP fails confusingly) |
| Plugins | all 18 shipped, drop-in `plugins.d/*.yaml`, `sandbox: false` |
| Desktop | Wayland (Hyprland), session D-Bus available to plugins |

Registration requires, per plugin: `VEYRON_JWT_SECRET` (frame-MAC key
derivation) and `VEYRON_JWT_TOKEN` minted with **`sub == plugin_id`** and
the plugin's declared permissions — the supervisor injects neither; both go
through each drop-in's `env:` list. Without them a plugin crash-loops
silently until the restart budget runs out (see "Ops/DX findings").

## Functional matrix — 27 OK / 7 not-OK (34 calls)

| Plugin | Result |
|---|---|
| ping-pong | OK (`{"reply":"pong"}`) — but response `status=0` (defect #3) |
| network | OK: real HTTPS GET 391–484 ms; `network_stats` OK |
| database | OK: set/get/incr/query roundtrips |
| notes / calendar | OK: create/list/update/delete flows |
| secrets | OK: value roundtrip verified |
| sync / sync-client | OK: set/snapshot/get_state |
| notify | OK: real desktop notification delivered (~13–18 ms) |
| gated-write | OK: `request_write` accepted |
| media | OK: `media_list_players` |
| system | OK: `sys_info`/`sys_procs`; `sys_battery` → correct structured error on desktop (no battery) |
| ai | OK: `list_agents` (cloud calls untested — no API keys) |
| filesystem | write/read OK; `/etc/shadow` → `ERR_FILES_PATH_ESCAPES_ROOT` — root allowlist holds |
| search | structured vault-first error without API key (expected behavior) |
| clipboard | providers/read OK; `clipboard_write` hangs 35–60+ s (**defect #1**) |
| tts / stt | local sherpa actions hang indefinitely (**defect #2**) |

Security posture confirmed live: default-deny IPC (caller without
`PERMISSION_IPC_SEND` → instant structured error), per-target
`ipc_targets` exact-match enforcement, filesystem root escape denial,
vault-first key resolution errors without leaking anything.

## Defects

### 1. `clipboard.clipboard_write` latency 35–60+ s — **fixed**

Read path is fine (3–22 ms); host binaries work instantly from a shell
(`wl-copy`/`wl-paste` under Hyprland). The plugin-side wait after spawning
`wl-copy` stretches to tens of seconds — consistent with the daemonizing
`wl-copy` child interacting badly with the spawn/wait logic. Reproducible
across runs; latency varies (35.8 s once, >60 s another time).
Tracked in `plugins/clipboard/ROADMAP.md`.

**Fix** (`fix/live-audit-defects`): `RealRunner::run` now wraps the whole
spawn → stdin → wait sequence in a real `tokio::time::timeout` (previously
the timeout was applied *after* the join, so it never fired), sets
`kill_on_drop(true)`, and for writers (`wl-copy`/`xclip -in`) completes on
direct-child exit instead of waiting for pipe EOF — the forked daemon
inherits the pipe copies and EOF never arrives. After the fix `clipboard_write`
completes in ~25 ms (live Wayland roundtrip verified) and `clipboard_read`
stays at 2–4 ms. Four new `RealRunner` regression tests lock the behavior
(`daemonized writer`, `timeout bound`, `stderr preserved`, `read stdout`).

### 2. tts/stt local sherpa path deadlocks before model load — **fixed**

`tts_voices`, `tts_synthesize` (sherpa/piper ru medium),
`stt_models`, `stt_transcribe` (zipformer ru int8, bundled test wav)
never respond. The plugin process sleeps — **0% CPU, RSS flat at ~22 MB**
— so the ONNX runtime never even begins loading. The kernel's pending-action
deadline fires `ACTION_TIMEOUT` at ~200 s. Both plugins, 100% repro.
Cloud providers untested (no keys). Tracked in
`plugins/tts/ROADMAP.md` and `plugins/stt/ROADMAP.md`.

**Fix** (`fix/live-audit-defects`): all blocking sherpa calls
(`voices`/`synthesize`/`synthesize_samples`/`transcribe`/`models`/`transcribe_pcm`)
are now dispatched via `tokio::task::spawn_blocking` so they no longer block
the async serve loop's worker thread. The underlying `OnceLock` engine init
and the heavy `OfflineTts`/`OfflineRecognizer::create` ONNX loads run on the
dedicated blocking pool. Isolated probes (including release builds) with the
bundled `piper-ru_RU-denis-medium` and `zipformer-ru-int8` models now return
`voices`/`models` instantly and synthesize/transcribe within seconds.

### 3. ping-pong replies with `status=0` — **fixed**

Payload is valid (`{"reply":"pong"}`) but `ActionStatus` arrives as 0 —
consistent with pre-M9 enum numbering (`ACTION_OK` used to be 0; M9 made
`*_UNKNOWN = 0`). Rebuild `ping-pong-rs` against current `veyron-wire`
and assert the status in its tests.

**Fix** (`fix/live-audit-defects`): source already used
`ActionStatus::ActionOk` against `vynkor-wire` 0.0.2 (post-M9 `ACTION_OK=1`);
the stale release binary is superseded at next `package.sh` run. Two new
unit tests lock the contract (`pong_replies_action_ok_not_zero`,
`unknown_action_replies_not_found`).

## Resource profile

Idle RSS (CPU 0% across the board):

| proc | RSS | proc | RSS |
|---|---|---|---|
| tts / stt / system | ~22.5 MB | media | 6.5 MB |
| vyn (kernel) | 20.2 MB | ai | 6.3 MB |
| network | ~7.7 MB | notes / calendar / sync-client | ~4.2 MB |
| database | ~7.7 MB | filesystem | 4.1 MB |
| sync | ~6.8 MB | secrets / notify / clipboard / search / gated-write | ~3.6–4.0 MB |

Whole stack ≈ **165 MB RSS**; kernel alone ≈ 20 MB.

Load phase: 125 database ops + 5 HTTPS GET in **1.1 s**, avg **1.13 ms/op**,
zero errors; RSS delta ≤ 0.5 MB anywhere; total CPU: database 0.06 s,
vyn 0.05 s, network 0.00 s. No growth, no leaks under burst.

## Ops/DX findings

These cost the most audit time and are worth fixing in docs/tooling:

1. **Secured-kernel plugin bootstrap is undocumented and silent-failing.**
   The supervisor does not inject `VEYRON_JWT_SECRET` or a per-plugin JWT;
   the operator must add both via drop-in `env:`. Failure mode: connect →
   register-reject → exit → restart loop, with an empty log ring buffer and
   nothing kernel-side at WARN. Deserves: README/docs coverage, a
   supervisor option to auto-mint tokens from the operator grant, and a
   loud rejection log line.
2. **`ipc_targets` has no wildcard** — exact-match only. An external
   client must list every target slug explicitly.
3. **External WS callers must target `kernel`** with the `ActionRequest`
   envelope. Targeting a plugin slug directly hits the zero-parse forward
   path: the request reaches the plugin verbatim, the reply carries the
   original `action_id`, finds no pending entry, and is dropped ("no
   matching pending request"). Kernel-side routing (pending registry,
   internal `kact-*` correlation, T-19 checks) only engages for
   target=`kernel`.
4. **HTTP API serves TLS by default** — plain-HTTP probes fail with a
   cryptic "Received HTTP/0.9". Worth a line in the kernel README.

## Scope notes

- Cloud provider calls (ai/tts/stt openai+elevenlabs, brave search) not
  exercised end-to-end — no API keys in the audit env; their structured
  no-key errors verified instead.
- `system.sys_lock` deliberately never invoked (would lock the desktop).
- stt model env quirk: zipformer models require
  `STT_PLUGIN_LOCAL_MODEL_TYPE=transducer` (a literal `zipformer` value is
  rejected by design).
