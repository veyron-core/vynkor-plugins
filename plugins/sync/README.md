# sync plugin

Host-side versioned KV state store for Vynkor plugins (SQLite-backed),
gated by `PERMISSION_STORAGE` and publishing a `sync.delta` event on every
mutation (`PERMISSION_EVENT_PUBLISH`). This is the D-13 host side: clients
pull a snapshot then subscribe to deltas; on reconnect they re-pull the
snapshot. See `docs/REMOTE_DEVICES_PLAN.md` §11 ("Sync") and the D-13 task
in `docs/REMOTE_DEVICES_ROADMAP.md` in the `vynkor` (kernel) repo.

Unlike `database`, there is **one shared store** — a single `sync.db` file
under `data_dir` — not a per-caller namespace. Identity is still validated:
an empty `caller_plugin_id` is an `ACTION_ERROR` (the caller id is
kernel-stamped and required for every action, even though it does not
select a namespace).

## Actions

| Action | Params | Result |
|---|---|---|
| `sync_get_snapshot` | `{}` | `{version, state: {key: value, ...}}` |
| `sync_get` | `{key}` | `{found, value}` |
| `sync_set` | `{key, value}` | `{ok: true, version}` (+ `sync.delta` event) |
| `sync_del` | `{key}` | `{deleted, version}` (+ `sync.delta` event) |

Values are arbitrary JSON, stored as JSON text (like `database`'s `db_set`);
`sync_get`/`sync_get_snapshot` return the decoded JSON value, not the raw
string.

`sync_set` and `sync_del` each bump the store's monotonic version by one in
a single SQLite transaction, and publish a `sync.delta` event. `sync_del` of
a key that does not exist is a no-op: `deleted: false`, the version is
unchanged, and no delta is published (there is no mutation to replicate).

## Permissions

The manifest declares `PERMISSION_STORAGE` (SQLite persistence) and
`PERMISSION_EVENT_PUBLISH` (the `sync.delta` event). There are **no
per-action `permission` gates** on the four actions: the manifest v2 schema
allows a per-action `permission` string, but the kernel's IPC router already
gates every action on the publisher's manifest `permissions` (R5-07), so a
second per-action field would be redundant — and, worse, a drift vector if
the two ever disagreed. Actions are unrestricted beyond those IPC gates.

## Delta event

Every mutation publishes an event of type `sync.delta`; the kernel
namespaces it to `plugin.sync.sync.delta` before delivery, so subscribers
must watch the fully-qualified type. The `payload_json` is one JSON object:

```json
{
  "op": "set",
  "key": "foo",
  "value": {"n": 1},
  "version": 7,
  "updated_at": 1723680000000
}
```

- `op` is `"set"` or `"del"`.
- `value` is the written JSON value for `op: "set"`, and `null` for
  `op: "del"`.
- `version` is the store version **after** this mutation.
- `updated_at` is the mutation's unix-millis timestamp.

Publishing is best-effort and fire-and-forget: the `ActionResponse` is
always sent first, and a dropped event never delays or fails the caller's
reply.

## Heartbeat TTL

Client plugins publish liveness state under `heartbeat.<device_id>` keys
(e.g. via `sync_set`). Stale heartbeat rows are lazily pruned on
`sync_set` and `sync_get_snapshot` only: rows where `key LIKE 'heartbeat.%'`
and `updated_at < now - ttl_secs*1000` are deleted, each producing a
`sync.delta` `op: "del"` event. Prune deltas are emitted **before** the
mutation's own delta (versions ascending). `SYNC_PLUGIN_HEARTBEAT_TTL_SECS`
defaults to `300`; `0` disables pruning.

The prune runs in the same transaction as the mutation that triggered it, so
the version bump and the row deletions commit atomically and the deltas a
subscriber receives are always a consistent prefix of the store's history.

## Config

Config is read from the environment (the kernel's plugin supervisor
translates the `config.yaml` `env:` entry into these before spawning the
plugin). Copy `config.example.yaml` for the documented defaults.

| Env var | Default | Meaning |
|---|---|---|
| `SYNC_PLUGIN_DATA_DIR` | — (required) | directory holding `sync.db` |
| `SYNC_PLUGIN_POOL_SIZE` | `4` | connections in the SQLite pool |
| `SYNC_PLUGIN_BUSY_TIMEOUT_MS` | `5000` | SQLite busy timeout |
| `SYNC_PLUGIN_MAX_VALUE_BYTES` | `1048576` (1 MiB) | `sync_set` value size cap |
| `SYNC_PLUGIN_MAX_RESPONSE_BYTES` | `4194304` (4 MiB) | `sync_get_snapshot` result size cap |
| `SYNC_PLUGIN_MAX_DB_BYTES` | `268435456` (256 MiB) | hard disk quota; `0` disables |
| `SYNC_PLUGIN_HEARTBEAT_TTL_SECS` | `300` | `heartbeat.*` row TTL; `0` disables |

Size caps reject — they never truncate. An oversized `sync_set` value or
`sync_get_snapshot` result is an `ACTION_ERROR`. `max_db_bytes` is a
different, stronger guarantee: enforced by SQLite itself via
`PRAGMA max_page_count`, any write that would grow `sync.db` past the
ceiling fails with `SQLITE_FULL`, no matter which action issued it.

## Concurrency

Hot-path plugin, so it drives the SDK's concurrent message loop
(`ConcurrentHandler` + `serve_concurrent`, `vynkor-sdk` ≥ 0.1.4): one task
owns the `VynkorClient` and `tokio::select!`s between inbound frames and an
mpsc channel of completed responses that spawned handler tasks push into.
The client is never behind a lock, so a handler replying can't deadlock
against the loop parked in `recv()`, and a panicking handler becomes an
`ACTION_ERROR` response instead of a dropped reply. The single shared
`sqlx::SqlitePool` (WAL mode) gives real parallelism; mutation + version
bump commit in one transaction, so concurrent writers serialize on the
SQLite write lock instead of racing the version counter. Replies may come
back out of order — the kernel matches on `action_id`.

## Status

v0.1. Depends on kernel support for `ActionRequest.caller_plugin_id`,
`PERMISSION_STORAGE`, and `PERMISSION_EVENT_PUBLISH`. The manifest declares
the published requirements (`vynkor-sdk = "0.1"`, `vynkor-wire = "0.2"`),
which resolve from crates.io. The client side (heartbeat publication,
snapshot pull, delta subscription) is a separate `sync-client` plugin
written against this exact contract.
