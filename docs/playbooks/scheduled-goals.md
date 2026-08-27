# AGT-08: Scheduled Goals — documented pattern (already works)

No new code needed — `scheduler` dispatch → `goal_start` already works today. Only recipe + examples needed.

## How

`scheduler` supports `action` mode: `schedule_set {cron|once, action: {name, params}}`. Set `name: "goal_start"`:

```json
{
  "action": "schedule_set",
  "params": {
    "id": "morning-briefing",
    "cron": {"expr": "30 7 * * 1-5", "tz": "Europe/Berlin"},
    "action": {"name": "goal_start", "params": {"goal": "morning briefing: weather 55.75,37.61 + today's meetings + speak with voice"}}
  }
}
```

Without `tz` — UTC (EXI-07). With `tz` — DST-aware.

Other examples:

```json
// one-shot in 10 seconds (test)
{"once": {"delay_ms": 10000}, "action": {"name": "goal_start", "params": {"goal": "remind to buy milk"}}}

// every Friday at 17:00 — report
{"cron": {"expr": "0 17 * * 5", "tz": "Europe/Berlin"}, "action": {"name": "goal_start", "params": {"goal": "make weekly report from tasks"}}}
```

## How it works

1. `scheduler` stores `sched:<id>` in `database`, scans every `SCHEDULER_PLUGIN_SCAN_SECS` (default 30).
2. On due time publishes `plugin.scheduler.fired` **and** does kernel-routed `goal_start`.
3. `agent` solves the goal as usual (allowlist + quotas + confirmation).

Gate T-19: `scheduler` calls `goal_start` like any external client — needs `PERMISSION_STORAGE` at `scheduler` (already has) and `PERMISSION_NETWORK/...` at `agent` for tool dispatch, but not at `scheduler`.

## Operator helpers

```bash
# create daily briefing
scripts/vyn-act goal_start '{"goal":"morning briefing"}' # one-shot test

# via scheduler (automation alternative)
# automations rule: plugin.scheduler.fired → goal_start also works,
# but direct action in scheduler is simpler (fewer hops)
```

## Playbooks on top

- `PLAY-02` smart-alarm already uses this pattern (cron → sound_play + briefing).
- `PLAY-01` sleep timer — one-shot `delay_ms` → `sound_stop`.

## Limitations

- Cron — 5/6 fields, fixed IANA tz (chrono-tz), no RRULE.
- Disabled schedules (`enabled:false`) do not fire but are kept.
- Gated targets (e.g., `notify_send` without `PERMISSION_NOTIFY` at agent) fail into `last_error` of the schedule — operator must grant `agent`.
