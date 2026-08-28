# sync-client plugin

The D-13 client side: a local mirror of the host `sync` plugin's store,
plus a periodic heartbeat. On every (re)connect it subscribes to the host's
delta events, pulls a `sync_get_snapshot` to seed the mirror, then applies
deltas as they arrive. The heartbeat pushes this device's liveness into the
host's sync state via `sync_set`, and reconnect re-pulls the snapshot so an
offline device catches up.

The mirror is `Arc<RwLock<Mirror>>` (version + key/value map). A snapshot
**replaces** the whole mirror — keys deleted while the device was offline
disappear. Deltas are applied in order; a delta whose `version` is at or
below the mirror's current version is skipped as already covered (correct
because per-connection event delivery is in-order).

## Actions

| Action | Params | Result |
|---|---|---|
| `sync_client_get_state` | `{}` (or empty) | `{version, state}` — the local mirror's store version and key→value map |

## Shared contract (with the `sync` plugin)

This plugin **consumes** the host `sync` plugin; it is not the store itself.
The contract is fixed and shared with the `sync` plugin:

- `sync_get_snapshot` — params `{}`, result `{"version": <int>, "state": {key: value, ...}}`.
- `sync_set` — params `{"key": "k", "value": <json>}`, result `{"ok": true, "version": <int>}`. This is what the heartbeat uses.
- Delta events — namespaced type `plugin.sync.sync.delta`, payload
  `{"op": "set"|"del", "key": "<key>", "value": <json|null>, "version": <int>, "updated_at": <unix-millis>}`
  where `version` is the store version *after* the mutation and `value` is
  null for `op: "del"`.

Because these are cross-plugin actions, the kernel's default-deny IPC model
applies on the **caller** side: this plugin declares
`PERMISSION_IPC_SEND` and lists `sync` in its `ipc_targets` allowlist (T-04).
Without both, the kernel rejects the `sync_get_snapshot`/`sync_set` sends.
The heartbeat deliberately goes through `sync_set` (an action), not
`publish_event` — the host sync plugin folds the value into its store and
re-broadcasts it as a delta, so every subscriber (including this client's
own mirror) stays consistent.

## Reconnect & heartbeat semantics

- The binary loops reconnecting with exponential backoff (1 s → 30 s).
- Each `serve_cycle` re-registers, re-subscribes to `plugin.sync.sync.delta`,
  and re-pulls the snapshot — the authoritative reset is what catches the
  mirror up after an offline gap.
- The heartbeat task fires every `SYNC_CLIENT_HEARTBEAT_SECS`, writing
  `heartbeat.<device_id>` = `{"ts", "device_id", "version"}` into the host
  store (fire-and-forget; responses are ignored). `0` disables it.
- Snapshot pull failure is best-effort: the stale mirror is kept rather than
  wiped, and the plugin continues to serve reads.

## Config

Config is read from the environment (the kernel's plugin supervisor
translates the `config.yaml` `env:` entry into these before spawning the
plugin). Copy `config.example.yaml` for the documented defaults.

| Env var | Default | Meaning |
|---|---|---|
| `SYNC_CLIENT_HEARTBEAT_SECS` | `30` | heartbeat interval in seconds; `0` disables the heartbeat task |
| `SYNC_CLIENT_DEVICE_ID` | `$HOSTNAME` → `"local"` | device id used in the heartbeat key (`heartbeat.<device_id>`) |
| `SYNC_CLIENT_SNAPSHOT_TIMEOUT_MS` | `5000` | timeout for the `sync_get_snapshot` pull at (re)connect |

## Known limitation (v1)

In a two-kernel deployment the kernel bridge (`device.<cap>` registrations)
does not yet relay host→client event subscriptions, so cross-kernel delta
delivery needs a kernel follow-up. Single-kernel deployments (the `sync` and
`sync-client` plugins on one kernel) work fully. This plugin implements the
correct protocol subscription regardless — the wire contract is in place for
when the bridge relays subscriptions.

## Status

v1. Depends on the host `sync` plugin being registered and running, and on
kernel support for `PERMISSION_SCHEDULER` / `PERMISSION_IPC_SEND` +
`ipc_targets` (T-04). The manifest declares the published requirements
(`vynkor-sdk = "0.1"`, `vynkor-wire = "0.2"`), which resolve from crates.io.
