# notes plugin

Note CRUD as a thin schema layer over the [`database`](../database/) plugin.
Every note is one JSON document stored under `note:<id>` in this plugin's own
private `database` namespace; ids come from an atomic counter (`db_incr` on
`meta:next_id`). No local state — restart-safe by construction.

Callers need **no storage permission**: `notes` is the plugin holding
`PERMISSION_STORAGE`. The kernel's T-19 anti-laundering check requires the
caller of a gated action to hold its permission, so the storage permission
lives here — and the `note_*` actions carry no per-action permission of their
own. Agents and clients get structured CRUD without raw storage access.

## Actions

| Action | Params | Result |
|---|---|---|
| `note_create` | `{title?, body?, tags?[]}` | `{id, note}` |
| `note_get` | `{id}` | `{found, note?}` |
| `note_list` | `{tag?, limit?, offset?}` | `{notes: [...], total}` |
| `note_update` | `{id, title?, body?, tags?}` | `{updated, note}` |
| `note_delete` | `{id}` | `{deleted}` |

At least one of `title`/`body` must be non-empty on create; an update may not
leave both empty. A note document:

```json
{
  "id": "1",
  "title": "Shopping",
  "body": "milk, eggs",
  "tags": ["errands"],
  "created_at_ms": 1755700000000,
  "updated_at_ms": 1755700000000
}
```

```json
// note_create
{ "title": "Shopping", "body": "milk, eggs", "tags": [" errands "] }
→ { "id": "1", "note": { ... } }

// note_list — newest first (updated_at_ms desc, numeric-id desc tie-break);
// total counts after filtering, before pagination
{ "tag": "errands", "limit": 50, "offset": 0 }
→ { "notes": [ ... ], "total": 1 }

// note_update — partial patch; absent fields survive
{ "id": "1", "body": "milk, eggs, bread" }
→ { "updated": true, "note": { ... } }

// note_delete — idempotent
{ "id": "1" }
→ { "deleted": true }
```

Reading a missing note is `{"found": false}`, deleting a missing one is
`{"deleted": false}` — neither is an error. Updating a missing note IS an
error (`ACTION_ERROR`, "note not found").

## Change events

Every mutation publishes a best-effort `plugin.notes.changed` event to the
kernel event bus, sent AFTER the action response (the same contract as
`database`'s change events):

```json
{"op": "created", "id": "1"}
```

`op` is `created` | `updated` | `deleted`; deletes publish only when a note
was actually removed. Reads publish nothing.

## Validation limits

| Field | Cap |
|---|---|
| `title` | 512 bytes |
| `body` | 256 KiB — deliberate headroom under `database`'s default 1 MiB value cap, so oversized bodies are rejected here with a clear message |
| `tags` | ≤32 unique, each ≤64 bytes; trimmed, empties dropped, deduped preserving order |
| `limit` | 1..=500, default 100 |

Listing loads all note docs into memory (`db_keys` prefix scan +
`db_batch_get`) and filters/sorts in-process — comfortable at personal scale
(thousands of notes); see `ROADMAP.md` for the scale ceiling.

## Concurrency architecture

The serve loop owns the `VynkorClient` exclusively and is the single reader
of the connection — no inbound frame is ever discarded. Each inbound
`ActionRequest` is handled in a spawned task that reaches `database` through
a channel-fronted RPC proxy ([`Rpc`] in `src/lib.rs`); replies and
fire-and-forget change events flow back through an outbound channel the loop
drains. Direct `send_action` from handler tasks would discard kernel pings
arriving during a slow database round-trip — same rationale as sync-client's
custom loop.

## Config

Config is read from the environment (the kernel's plugin supervisor
translates the `config.yaml` `env:` entry into these before spawning the
plugin). Copy `config.example.yaml` for the documented defaults.

| Env var | Default | Meaning |
|---|---|---|
| `NOTES_PLUGIN_DB_TIMEOUT_MS` | `5000` | timeout for each `database` IPC call |

## Testing

`cargo test` — unit tests cover request parsing/validation; end-to-end tests
drive the real serve loop against a fake kernel over `UnixStream::pair`
(registration handshake, an in-memory KV answering the `db_*` calls, event
recording). No live kernel or `database` binary needed.

## Status

v0.1. Depends on the `database` plugin being registered and running. The
manifest declares `PERMISSION_STORAGE` + `PERMISSION_EVENT_PUBLISH`.
