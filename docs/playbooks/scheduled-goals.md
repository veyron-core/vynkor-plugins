# AGT-08: Scheduled Goals — документированный паттерн (уже работает)

No new code needed — `scheduler` dispatch → `goal_start` уже работает сегодня. Нужен только рецепт + примеры.

## Как

`scheduler` умеет `action`-режим: `schedule_set {cron|once, action: {name, params}}`. Укажите `name: "goal_start"`:

```json
{
  "action": "schedule_set",
  "params": {
    "id": "morning-briefing",
    "cron": {"expr": "30 7 * * 1-5", "tz": "Europe/Berlin"},
    "action": {"name": "goal_start", "params": {"goal": "утренний брифинг: погода 55.75,37.61 + встречи сегодня + скажи голосом"}}
  }
}
```

Без `tz` — UTC (EXI-07). С `tz` — DST-aware.

Другие примеры:

```json
// one-shot через 10 секунд (тест)
{"once": {"delay_ms": 10000}, "action": {"name": "goal_start", "params": {"goal": "напомни купить молоко"}}}

// каждую пятницу в 17:00 — отчёт
{"cron": {"expr": "0 17 * * 5", "tz": "Europe/Berlin"}, "action": {"name": "goal_start", "params": {"goal": "сделай weekly отчёт по задачам из tasks"}}}
```

## Как это работает

1. `scheduler` хранит `sched:<id>` в `database`, скан каждые `SCHEDULER_PLUGIN_SCAN_SECS` (default 30).
2. По наступлению срока публикует `plugin.scheduler.fired` **и** делает kernel-routed `goal_start`.
3. `agent` решает цель как обычно (allowlist + quotas + confirmation).

Гейт T-19: `scheduler` вызывает `goal_start` как любой внешний клиент — нужны права `PERMISSION_STORAGE` у `scheduler` (уже есть) и `PERMISSION_NETWORK/…` у `agent` для диспатча инструментов, но не у `scheduler`.

## Операторские хелперы

```bash
# создать daily briefing
scripts/vyn-act goal_start '{"goal":"утренний брифинг"}' # one-shot test

# через scheduler (automation альтернатива)
# automations правило: plugin.scheduler.fired → goal_start тоже работает,
# но прямой action в scheduler проще (меньше hops)
```

## Playbooks поверх

- `PLAY-02` smart-alarm уже использует этот паттерн (cron → sound_play + briefing).
- `PLAY-01` sleep timer — one-shot `delay_ms` → `sound_stop`.

## Ограничения

- Cron — 5/6 полей, фиксированная IANA tz (chrono-tz), без RRULE.
- Disabled schedules (`enabled:false`) не файрят, но сохраняются.
- Gated targets (например, `notify_send` без `PERMISSION_NOTIFY` у agent) фейлятся в `last_error` расписания — оператор должен грантовать `agent`.

