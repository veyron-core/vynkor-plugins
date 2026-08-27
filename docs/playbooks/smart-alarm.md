# PLAY-02: Smart Alarm — cron + rising volume + briefing

Cron (scheduler) → `sound_play` with rising volume → `PLAY-01` briefing (weather + calendar.due + rss).

## Why it replaces phone alarm

- Runs on host, not phone; works offline; `sound` is the only speaker owner.
- Volume ramp avoids harsh wake: 10% → 100% over 60s via two scheduled steps.
- Briefing after wake is `weather_forecast` + `calendar` + `sound_play` of TTS.

## Setup: 07:30 every weekday, Europe/Berlin

```json
// 1. Cron that fires at 07:30 Mon-Fri in IANA tz (EXI-07)
{
  "action": "schedule_set",
  "params": {
    "id": "alarm-0730-weekdays",
    "cron": {"expr": "30 7 * * 1-5", "tz": "Europe/Berlin"},
    "action": {"name": "sound_play", "params": {"path": "/home/user/alarm.mp3", "volume": 0.1}}
  }
}

// 2. One-shot 30s later to bump volume (or use sound_volume if supported)
{
  "action": "schedule_set",
  "params": {
    "id": "alarm-0730-vol2",
    "cron": {"expr": "30 7 * * 1-5", "tz": "Europe/Berlin"},
    "action": {"name": "sound_play", "params": {"path": "/home/user/alarm.mp3", "volume": 1.0}}
  }
}
```

Simpler: single alarm that triggers an `automations` rule which does the ramp:

```json
// automations rule: on scheduler fired -> two-step wake
{
  "action": "automation_create",
  "params": {
    "id": "smart-alarm",
    "trigger": {"event": "plugin.scheduler.fired", "conditions": [{"pointer": "/schedule_id", "equals": "alarm-0730-weekdays"}]},
    "action": {"name": "goal_start", "params": {"goal": "wake me up: play alarm.mp3 quietly, in 30s loudly, then give weather and today's meetings with voice"}}
  }
}
```

## Briefing goal (agent does TTS)

`goal_start {"goal": "morning briefing: weather_forecast 55.75,37.61 + today's calendar + speak with voice"}`

The agent:
1. `weather_forecast {"lat":55.75,"lon":37.61,"days":1,"timezone":"Europe/Moscow"}`
2. `calendar_list` (or `event_list`)
3. `tts_synthesize` → `sound_play` of the summary.

## Via `vyn ask`

```bash
vyn ask "set weekday alarm at 7:30 with rising volume and briefing"
```

## Notes

- Requires `scheduler` TZ support (EXI-07) for DST correctness; `tz_offset_min` breaks on DST.
- `sound_play` replace-on-play ensures the second play at 100% replaces the first clip.
- To snooze: `schedule_set {"once":{"delay_ms":300000},"action":{"name":"sound_play",...}}`.
- To disable: `schedule_delete {"id":"alarm-0730-weekdays"}`.
