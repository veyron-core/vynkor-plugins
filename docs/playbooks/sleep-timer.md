# PLAY-01: Sleep Timer — «подкаст на полчаса»

Two primitives: `sound_play` + `scheduler` one-shot → `sound_stop`. No code after `automations`/`scheduler` shipped.

## 1. One-shot via `scheduler` (simplest)

```json
// start playback
{ "action": "sound_play", "params": {"path": "/home/user/podcast.mp3"} }
// schedule stop in 30 min
{
  "action": "schedule_set",
  "params": {
    "id": "sleep-30m",
    "once": {"delay_ms": 1800000},
    "action": {"name": "sound_stop", "params": {}}
  }
}
```

Agent: `goal_start {"goal": "включи подкаст на полчаса"}` → does both calls.

## 2. As `automations` rule (reusable)

Create a rule that exposes `sleep_timer` as an action-like shortcut:

```json
{
  "action": "automation_create",
  "params": {
    "id": "sleep-timer",
    "trigger": {"event": "manual"},
    "action": {"name": "schedule_set", "params": {
      "once": {"delay_ms": 1800000},
      "action": {"name": "sound_stop"}
    }}
  }
}
```

Then `sound_play` + trigger the rule.

## 3. Via `vyn ask`

```bash
vyn ask "включи /home/user/sleep.mp3 на 20 минут и выключи"
```

Agent maps "на N минут" → `schedule_set` with `delay_ms = N*60000`.

## Notes

- No `PERMISSION_*` grant needed for `sound_stop` (same `PERMISSION_AUDIO` as `sound_play`; `scheduler` holds `STORAGE` only).
- To cancel: `schedule_delete {"id": "sleep-30m"}` or `sound_stop` immediately.
- For streaming radio: same pattern, `sound_play {url: "https://..."}` if `sound` supports URL (ffplay path).
