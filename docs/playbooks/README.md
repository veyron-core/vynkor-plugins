# Playbooks — zero-code compositions

All playbooks are compositions of shipped plugins via `scheduler`/`automations`/`agent`. No new plugin code needed after CAP-01.

| Playbook | File | Primitives |
|---|---|---|
| Sleep timer | [sleep-timer.md](sleep-timer.md) | `sound_play` + `scheduler` one-shot → `sound_stop` |
| Smart alarm | [smart-alarm.md](smart-alarm.md) | `scheduler` cron (IANA tz) + `sound_play` ramp + `weather` + `calendar` + `tts` |
| Scheduled goals | [scheduled-goals.md](scheduled-goals.md) | `scheduler` cron/one-shot → `goal_start` (AGT-08) |

Create more: `focus mode` (PLAY-03), `voice DJ` (PLAY-04), etc. — see `PLANS.md` PLAY section.
