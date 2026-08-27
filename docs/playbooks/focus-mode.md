# PLAY-03: Focus Mode — "do not disturb for an hour"

`notify` silent inbox + timer + `media` pause.

## What it does

- All `notify_send` go to `silent: true` inbox (already in `notify` v0.2: `notify_list`/`mark_read`/`delete`).
- `media_pause` for the focus period.
- After N minutes — restore.

## Manual variant (without automations)

```json
// 1. Enable focus for 60m: pause media
{"action": "media_pause", "params": {}}

// 2. All notify now silent — agent just sends with silent:true
{"action": "notify_send", "params": {"title": "Focus", "message": "Focus mode on 60m", "silent": true}}

// 3. Return timer
{
  "action": "schedule_set",
  "params": {
    "id": "focus-return",
    "once": {"delay_ms": 3600000},
    "action": {"name": "notify_send", "params": {"title": "Focus", "message": "Focus mode off"}}
  }
}
```

## As automations rule (recommended)

```json
{
  "action": "automation_create",
  "params": {
    "id": "focus-60m",
    "trigger": {"event": "manual"},
    "conditions": [],
    "action": {"name": "schedule_set", "params": {
      "once": {"delay_ms": 3600000},
      "event": {"payload": {"focus": "off"}}
    }}
  }
}
```

Trigger `focus` → `plugin.scheduler.fired` → second automation `media_play` + `notify_send`.

## Via `vyn ask`

```bash
vyn ask "do not disturb for an hour, pause music"
```

Agent: `media_pause` + `schedule_set 60m → media_play` + all notify silent.

## Cancel

```json
{"action": "schedule_delete", "params": {"id": "focus-return"}}
{"action": "media_play", "params": {}}
```
