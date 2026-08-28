# database plugin

Per-caller-namespaced KV + raw SQL storage for Vynkor plugins, gated by
`PERMISSION_STORAGE`. One SQLite file per calling plugin — callers cannot
see or query each other's data. See
`docs/superpowers/specs/2026-07-15-database-plugin-design.md` for the full
design and `ROADMAP.md` in this directory for what's deferred.

Identity is taken only from the kernel-stamped `ActionRequest.caller_plugin_id`,
never from params, and is sanitized to `[a-zA-Z0-9_-]` before use as a
filename — an empty or malformed caller id is an `ACTION_ERROR`, never a
shared/default namespace.

## Actions

| Action | Params | Result |
|---|---|---|
| `db_get` | `{key}` | `{found, value}` |
| `db_set` | `{key, value, ttl_ms?}` | `{ok: true}` |
| `db_delete` | `{key}` | `{deleted}` |
| `db_batch_get` | `{keys: [..]}` | `{values: {key: value, ..}}` (missing keys map to `null`) |
| `db_incr` | `{key, delta?}` | `{ok, value}` |
| `db_keys` | `{prefix?}` | `{keys: [..]}` |
| `db_append` | `{key, value}` | `{ok, length}` |
| `db_patch` | `{key, path, value}` | `{ok, value}` |
| `db_query` | `{sql, params: [..]}` | `{rows: [..], rows_affected}` |

`db_query` runs against the caller's own database file only — `ATTACH` is
rejected (whole-word, case-insensitive keyword pre-check). Positional binds
use `?1`, `?2`, …. Any statement that yields rows returns them in `rows`
(including `INSERT … RETURNING`), and every statement reports `rows_affected`
— there is no `starts_with("select")` guess, so row-producing writes are not
dropped.

Values are stored as JSON text; `db_get`/`db_batch_get` return the decoded
JSON value, not the raw string. A raw `db_query` against the `kv` table sees
that JSON text verbatim (a stored string comes back quoted, e.g. `"bar"`).

**See [`USAGE.md`](./USAGE.md)** for per-action request/response examples,
every error message a caller can hit, column/param type mapping, and common
patterns (building your own tables, transactions, TTL, recovering from a full
quota).

## KV TTL

`db_set` accepts an optional `ttl_ms`: the key expires that many milliseconds
after the set (`0`/negative = no expiry, same as omitting it). Expiry is
enforced two ways: an `expires_at` column (added by an idempotent schema
migration to `.db` files created before v0.3), swept with a `DELETE` before
**every** action — including raw `db_query` — plus expiry filters on the KV
accessors (`db_get`, `db_batch_get`) so an expired key reads as missing even
if the sweep hasn't run. The `expires_at` column is visible to raw SQL.

## Change events

Every mutation — `db_set`, `db_delete` (only when a row was actually
deleted), `db_incr`, `db_append`, `db_patch` — publishes a best-effort
`plugin.database.changed` event to the kernel event bus (the kernel prepends
the `plugin.<sender_id>.` namespace). The payload is one JSON object:

```json
{"caller": "notes", "action": "db_set", "key": "user:42"}
```

Reads (`db_get`, `db_batch_get`, `db_keys`, `db_query`) publish nothing.
Publishing is fire-and-forget, same as `network.request_completed`: the
`ActionResponse` is always sent first, and a dropped event never delays or
fails the caller's reply. Requires `PERMISSION_EVENT_PUBLISH`, which the
manifest declares.

## Config

Config is read from the environment (the kernel's plugin supervisor
translates the `config.yaml` `env:` entry into these before spawning the
plugin). Copy `config.example.yaml` for the documented defaults.

| Env var | Default | Meaning |
|---|---|---|
| `DATABASE_PLUGIN_DATA_DIR` | — (required) | directory holding per-caller `.db` files |
| `DATABASE_PLUGIN_POOL_SIZE` | `4` | connections per caller pool |
| `DATABASE_PLUGIN_BUSY_TIMEOUT_MS` | `5000` | SQLite busy timeout |
| `DATABASE_PLUGIN_MAX_VALUE_BYTES` | `1048576` (1 MiB) | `db_set` value size cap |
| `DATABASE_PLUGIN_MAX_RESPONSE_BYTES` | `4194304` (4 MiB) | `db_query`/`db_batch_get` result size cap |
| `DATABASE_PLUGIN_MAX_DB_BYTES` | `268435456` (256 MiB) | hard per-caller disk quota; `0` disables |

Size caps reject — they never truncate. An oversized `db_set` value or
`db_query`/`db_batch_get` result is an `ACTION_ERROR`.

The disk quota is a different, stronger guarantee: `max_value_bytes` and
`max_response_bytes` are application-layer checks that only bound one value or
one response, and a raw `db_query` write can sidestep `max_value_bytes`
entirely (`insert ... values (value || value)`, `zeroblob(...)`).
`max_db_bytes` is enforced by SQLite itself via `PRAGMA max_page_count`: any
write that would grow the caller's `.db` file past the ceiling fails with
`SQLITE_FULL`, no matter which action issued it. A full caller recovers by
freeing space (deleting rows); note SQLite does not return freed pages to the
OS without a `VACUUM`, so the file stays at its high-water mark but the freed
pages are reused for subsequent writes.

## Concurrency

Unlike `ai`/`tts`/`stt` (sequential `Plugin::run`), this is a hot-path
plugin, so it drives the SDK's concurrent message loop
(`ConcurrentHandler` + `serve_concurrent`, `vynkor-sdk` ≥ 0.1.4): one task
owns the `VynkorClient` and `tokio::select!`s between inbound frames and an
mpsc channel of completed responses that spawned handler tasks push into.
The client is never behind a lock, so a handler replying can't deadlock
against the loop parked in `recv()`, and a panicking handler becomes an
`ACTION_ERROR` response instead of a dropped reply. Each caller gets a
cached `sqlx::SqlitePool` (WAL mode) for real parallelism. Replies may come
back out of order — the kernel matches on `action_id`.

## Status

v1. Depends on kernel support for `ActionRequest.caller_plugin_id` and
`PERMISSION_STORAGE` (see
`docs/superpowers/plans/2026-07-17-database-plugin-kernel-support.md` in the
`vynkor` repo). The manifest declares the published requirements
(`vynkor-sdk = "0.1"`, `vynkor-wire = "0.2"`), which resolve from crates.io.
