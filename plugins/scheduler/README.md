# scheduler plugin

Once/cron schedules as a thin schema layer over the
[`database`](../database/) plugin. Every schedule is one JSON document
stored under `sched:<id>` in this plugin's own private `database` namespace;
generated ids come from an atomic counter (`db_incr` on `meta:next_id`). No
local state — restart-safe by construction.

Callers of the `schedule_*` actions need no permissions of their own. The
plugin itself holds `PERMISSION_STORAGE` (the kernel's T-19 anti-laundering
check requires the caller of a gated action to hold its permission — the
gated party here is `scheduler` calling `db_*`) and `PERMISSION_EVENT_PUBLISH`
(`fired`/`changed` events).

## Actions

| Action | Params | Result |
|---|---|---|
| `schedule_set` | `{id?, name?, enabled?, once \| cron, event \| action}` | `{id, created, schedule}` |
| `schedule_get` | `{id}` | `{found, schedule?}` |
| `schedule_list` | `{limit?, offset?}` | `{schedules: [...], total}` |
| `schedule_delete` | `{id}` | `{deleted}` |

A trigger and a fire kind are each mandatory, exactly one variant:

- **Trigger** — `once: {at_ms}` (absolute unix-ms) or `once: {delay_ms}`
  (resolved to an absolute `at_ms` at set time), or `cron: {expr,
  tz_offset_min?}` (standard 5-field crontab or 6-field with seconds).
- **Fire** — `event: {payload?}` publishes `plugin.scheduler.fired`, or
  `action: {name, params?}` performs a kernel-routed action call.

A schedule document:

```json
{
  "id": "backup-db",
  "name": "nightly backup",
  "enabled": true,
  "trigger": { "type": "cron", "expr": "0 3 * * *", "tz_offset_min": 180 },
  "fire": { "type": "action", "name": "notify_send", "params": {"title": "backup done"} },
  "fired_once": false,
  "fire_count": 0,
  "last_fired_ms": null,
  "last_error": null,
  "created_at_ms": 1755600000000,
  "updated_at_ms": 1755600000000
}
```

```jsonc
// schedule_set — one-shot after 30s; id generated when absent
{ "once": {"delay_ms": 30000}, "event": {"payload": {"job": "cleanup"}} }
→ { "id": "1", "created": true, "schedule": { ... } }

// schedule_set — client-supplied id doubles as the database key suffix;
// re-setting the same id replaces the document and resets fire state
{ "id": "backup-db", "cron": {"expr": "0 3 * * *", "tz_offset_min": 180},
  "action": {"name": "notify_send", "params": {"title": "backup"}} }
→ { "id": "backup-db", "created": false, "schedule": { ... } }

// schedule_list — soonest next_fire first; done/disabled (nothing pending)
// sort last. total counts before pagination.
{ "limit": 50 }
→ { "schedules": [ ... ], "total": 2 }

// schedule_delete — idempotent
{ "id": "backup-db" }
→ { "deleted": true }
```

Reading a missing schedule is `{"found": false}`, deleting a missing one is
`{"deleted": false}` — neither is an error. Cron expressions are validated
at `schedule_set` time; an invalid expression never reaches storage.

## Timing model

- **Scan loop:** every `SCHEDULER_PLUGIN_SCAN_SECS` (default 30; `0`
  disables scanning entirely). The first tick fires immediately, doubling as
  the startup catch-up. Firing precision is bounded by the scan interval.
- **At-most-once:** due one-shots are marked `fired_once` BEFORE dispatch.
  A crash between mark and dispatch loses that one fire; the reverse order
  would re-fire forever.
- **Late firing:** one-shots that came due while the plugin was down fire
  once on the next scan with `late: true`.
- **Cron anchoring:** recurring schedules anchor to their last scheduled
  fire (or creation for fresh docs): the next occurrence is computed
  strictly after that anchor. Missed occurrences during downtime are
  skipped — downtime collapses into at most one catch-up fire.
- **Timezones:** cron expressions evaluate in the fixed UTC offset given by
  `tz_offset_min` (−720..=840 minutes, default 0 = UTC). Named IANA zones
  are a non-goal for v0.1 (see `ROADMAP.md`).
- **Disabled schedules** (`enabled: false`) persist but never fire and list
  with no pending deadline.

## Events

Every mutation publishes a best-effort `plugin.scheduler.changed` event AFTER
the action response (same contract as `database`):

```json
{"op": "created", "id": "1"}
```

Every fire publishes `plugin.scheduler.fired`:

```json
{
  "schedule_id": "backup-db",
  "name": "nightly backup",
  "scheduled_for_ms": 1755685200000,
  "late": false,
  "fire_count": 7,
  "fire": {"type": "action", "name": "notify_send", "params": {"title": "backup"}},
  "payload": null
}
```

`payload` echoes the schedule's configured event payload (`null` for
action-mode schedules); `fire_count` counts this fire.

## Action-mode fires and permissions

Fired action calls are ordinary kernel-routed requests: the kernel resolves
the provider by action name and enforces T-19 against **scheduler's own**
permission claims. Ungated targets work out of the box (e.g. calling another
wrapper plugin's CRUD action). Gated targets fail with a kernel permission
error recorded in the schedule's `last_error` unless the operator grants
`scheduler` the corresponding permission deliberately — by design there is no
way to launder a permission through a schedule. A failed dispatch never
aborts the scan; it lands in `last_error` (truncated to 256 bytes) and the
next occurrence proceeds normally.

## Concurrency architecture

The serve loop owns the `VynkorClient` exclusively and is the single reader
of the connection — no inbound frame is ever discarded. Action handlers AND
the periodic scan run as spawned tasks communicating through a channel-fronted
RPC proxy ([`Rpc`] in `src/lib.rs`) plus an outbound channel for replies and
events. This matters precisely because of the timer: a scan started by a tick
uses several sequential IPC round-trips, and direct `send_action` would
silently discard user requests arriving mid-scan (same rationale as
calendar/sync-client).

## Validation limits

| Field | Cap |
|---|---|
| `id` | ≤64 bytes, `[A-Za-z0-9_-]` only |
| `name` | 256 bytes, non-empty when present |
| `cron.expr` | 128 bytes; must parse at set time |
| `tz_offset_min` | −720..=840 |
| `once.delay_ms` | 0..=315,360,000,000 (10 years) |
| `once.at_ms` | positive unix-ms |
| `event.payload` / `action.params` | ≤65,536 bytes serialized |
| `action.name` | ≤128 bytes, non-empty |
| `limit` | 1..=500, default 100 |

Listing loads all schedule docs into memory (`db_keys` prefix scan +
`db_batch_get`) and sorts/filters in-process — comfortable at personal scale;
see `ROADMAP.md`.

## Testing

`cargo test` — unit tests cover request validation, cron normalization
(5-field prefixing), offset-aware next-occurrence math and the pure
due-selection logic (including the fired/disabled exclusions); e2e tests
drive the real serve loop against a fake kernel over `UnixStream::pair`
(registration handshake, in-memory KV answering `db_*` calls, an action-call
recorder, event recording), including live-scan tests (`SCAN_SECS=1`)
verifying delayed fires, late-flag catch-up, at-most-once behavior, repeated
cron fires, action dispatch + `last_error` recording, and disabled-schedule
suppression. No live kernel needed.

## Status

v0.1. Depends on the `database` plugin being registered and running. Fired
action-mode schedules additionally require whichever plugin serves the named
action.
