# calendar plugin

Event CRUD plus reminders as a thin schema layer over the
[`database`](../database/) plugin. Every event is one JSON document stored
under `event:<id>` in this plugin's own private `database` namespace; ids
come from an atomic counter (`db_incr` on `meta:next_id`). No local state —
restart-safe by construction.

Callers of the `event_*` actions need no permissions of their own. The
plugin itself holds `PERMISSION_STORAGE` (the kernel's T-19 anti-laundering
check requires the caller of a gated action to hold its permission — the
gated party here is `calendar` calling `db_*`), `PERMISSION_EVENT_PUBLISH`
(`changed`/`due` events), and `PERMISSION_NOTIFY` (best-effort reminder
delivery through the [`notify`](../notify/) plugin's gated `notify_send`).

## Actions

| Action | Params | Result |
|---|---|---|
| `event_create` | `{title, description?, start_ms, end_ms?, all_day?, remind_before_ms?, tags?[]}` | `{id, event}` |
| `event_get` | `{id}` | `{found, event?}` |
| `event_list` | `{from_ms?, to_ms?, tag?, limit?, offset?}` | `{events: [...], total}` |
| `event_update` | `{id, title?, description?, start_ms?, end_ms?, all_day?, remind_before_ms?, tags?}` | `{updated, event}` |
| `event_delete` | `{id}` | `{deleted}` |
| `calendar_ics_import` | `{ics_base64}` | `{imported, updated, parsed, window_days, event_ids}` |
| `calendar_ics_export` | `{}` | `{ics_base64}` |

An event document:

```json
{
  "id": "1",
  "title": "Dentist",
  "description": "checkup",
  "start_ms": 1755700000000,
  "end_ms": 1755703600000,
  "all_day": false,
  "remind_before_ms": 900000,
  "reminder_fired": false,
  "tags": ["health"],
  "created_at_ms": 1755600000000,
  "updated_at_ms": 1755600000000
}
```

```json
// event_create — title required non-empty; start_ms positive unix-ms (UTC);
// end_ms >= start_ms when present
{ "title": "Dentist", "start_ms": 1755700000000, "end_ms": 1755703600000,
  "remind_before_ms": 900000, "tags": ["health"] }
→ { "id": "1", "event": { ... } }

// event_list — sorted by start_ms ascending (numeric-id tie-break);
// from_ms/to_ms filter inclusively on start_ms; total counts after
// filtering, before pagination
{ "from_ms": 1755700000000, "to_ms": 1755786400000, "limit": 50 }
→ { "events": [ ... ], "total": 1 }

// event_update — partial patch; changing start_ms/end_ms/remind_before_ms
// resets reminder_fired so the new schedule reminds again
{ "id": "1", "start_ms": 1755703600000 }
→ { "updated": true, "event": { ... } }

// event_delete — idempotent
{ "id": "1" }
→ { "deleted": true }
```

Reading a missing event is `{"found": false}`, deleting a missing one is
`{"deleted": false}` — neither is an error. Updating a missing event IS an
error (`ACTION_ERROR`, "event not found").

## ICS import / export (v0.2)

`calendar_ics_import {ics_base64}` decodes and parses a VCALENDAR payload
and upserts VEVENTs keyed by their `UID` (stored as `ics_uid`) — re-importing
the same calendar updates in place instead of duplicating. Only occurrences
inside the window `[now - 1d, now + 90d]` are materialized; RRULEs expand to
concrete events for `FREQ=DAILY/WEEKLY/MONTHLY` with `INTERVAL`, `COUNT`,
`UNTIL` and weekly `BYDAY`. `YEARLY` rules keep just the first occurrence.
VALARM relative triggers (`TRIGGER:-PT15M`) become `remind_before_ms`;
LOCATION folds into the first description line. TZID values are read as
naive wall-clock in UTC (documented v1 limitation). Payload cap ~6 MiB,
hard cap 500 documents per import.

`calendar_ics_export {}` returns every stored event as one base64-encoded
VCALENDAR (UTC times; all-day events use `VALUE=DATE`; reminders emit a
DISPLAY VALARM). Round-trips through the importer.

## Reminder model

- **Opt-in:** only events with `remind_before_ms` set participate
  (`0` = fire exactly at `start_ms`; cap 10 years).
- **Scan loop:** every `CALENDAR_PLUGIN_SCAN_SECS` (default 30; `0`
  disables scanning entirely). The first tick fires immediately, doubling as
  the startup catch-up.
- **At-most-once:** a due reminder is marked `reminder_fired` BEFORE the
  notification goes out. A crash between mark and publish loses one
  notification; the reverse order would re-fire forever.
- **Late firing:** reminders that came due while the plugin was down fire
  once on the next scan with `late: true`. There is no maximum-lateness
  cutoff yet (see `ROADMAP.md`).
- **Reschedule resets:** updating `start_ms`/`end_ms`/`remind_before_ms`
  clears `reminder_fired` — the new schedule deserves a new notification.

## Events

Every mutation publishes a best-effort `plugin.calendar.changed` event AFTER
the action response (same contract as `database`):

```json
{"op": "created", "id": "1"}
```

Every fired reminder publishes `plugin.calendar.due`:

```json
{"event_id": "1", "title": "Dentist", "start_ms": 1755700000000,
 "remind_at_ms": 1755699100000, "late": false}
```

## notify integration

With `CALENDAR_PLUGIN_NOTIFY=true` (default), each fired reminder also calls
the `notify` plugin's `notify_send` with `{"title": "Calendar", "message":
"<title> starts ..."}`. Best-effort: a failed delivery is logged and never
fails the scan or any action response. The `notify` plugin must be
registered and running for this path; without it the `due` events still
publish.

## Concurrency architecture

The serve loop owns the `VeyronClient` exclusively and is the single reader
of the connection — no inbound frame is ever discarded. Action handlers AND
the periodic reminder scan run as spawned tasks communicating through a
channel-fronted RPC proxy ([`Rpc`] in `src/lib.rs`) plus an outbound channel
for replies and events. This matters precisely because of the timer: a scan
started by a tick uses several sequential IPC round-trips, and direct
`send_action` would silently discard user requests arriving mid-scan (same
rationale as sync-client's custom loop).

## Validation limits

| Field | Cap |
|---|---|
| `title` | 512 bytes, non-empty |
| `description` | 4096 bytes |
| `tags` | ≤32 unique, each ≤64 bytes; trimmed, empties dropped, deduped preserving order |
| `limit` | 1..=500, default 100 |
| `remind_before_ms` | 0..=315,360,000,000 (10 years) |

Listing loads all event docs into memory (`db_keys` prefix scan +
`db_batch_get`) and filters/sorts in-process — comfortable at personal scale;
see `ROADMAP.md`.

## Testing

`cargo test` — unit tests cover request parsing/validation and the pure
reminder-selection logic (including the saturating overflow guard); e2e
tests drive the real serve loop against a fake kernel over
`UnixStream::pair` (registration handshake, in-memory KV answering `db_*`
calls, a `notify_send` recorder, event recording), including a live-scan
test (`SCAN_SECS=1`) verifying at-most-once firing, notification delivery
and the fired-flag reset on reschedule. No live kernel needed.

## Status

v0.1. Depends on the `database` plugin being registered and running;
`notify` is needed only when `CALENDAR_PLUGIN_NOTIFY=true`.
