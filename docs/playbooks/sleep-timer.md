# PLAY-01: Sleep Timer — "podcast for 30 minutes"

Two primitives: `sound_play` + `scheduler` one-shot → `sound_stop`. No code needed after `automations`/`scheduler`.

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

Agent: `goal_start {"goal": "play podcast for 30 minutes"}` → does both calls.

## 2. As `automations` rule (reusable)

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
vyn ask "play /home/user/sleep.mp3 for 20 minutes then stop"
```

Agent maps "for N minutes" → `schedule_set` with `delay_ms = N*60000`.

## Notes

- No extra permission needed for `sound_stop` (same `PERMISSION_AUDIO` as `sound_play`).
- To cancel: `schedule_delete {"id": "sleep-30m"}` or `sound_stop` immediately.
- For streaming radio: same pattern, `sound_play {url: "https://..."}` if `sound` supports URL (ffplay path).
