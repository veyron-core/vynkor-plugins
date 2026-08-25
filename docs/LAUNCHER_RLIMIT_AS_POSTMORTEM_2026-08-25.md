# Постмортем: launcher vs RLIMIT_AS — «launched:true, но окна нет»

**Дата:** 2026-08-25 · **Затронуто:** launcher 0.1.0 → 0.1.4 · **Статус:** исправлено и верифицировано
**Симптом-жертва:** Firefox (проявлялся на любом «толстом» GUI-приложении; Alacritty работала и маскировала проблему)

---

## TL;DR

Голосовой запуск приложений через ядро сообщал `launched:true`, но окно не появлялось,
а Firefox после такого запуска показывал «вкладка упала / restore tabs». Причин оказалось
**три слоя**, каждый из которых по отдельности давал ровно этот симптом:

1. **Ядро вешает `RLIMIT_AS` на каждый плагин** (даже при `sandbox: false`) — плагин жил
   с капом виртуальной памяти (512 МБ по дефолту → позже 1 ГБ из drop-in).
2. **Дети наследуют этот кап**, а поднять его нельзя: непривилегированный процесс не может
   увеличить **hard limit** (`setrlimit(AS→∞)` = EPERM). Значит любые попытки «сбросить лимит
   у себя перед спавном» неработоспособны в принципе.
3. **`systemd-run --scope` не спасает**: scope-юнит исполняет цель форком самого клиента
   systemd-run → полный набор rlimits наследуется от плагина.

Итоговое решение (launcher 0.1.4): спавн через **transient service**
(`systemd-run --user --collect --property=KillMode=process`) + явный проброс GUI-env через
`--setenv`. Сервис форкается PID1'ом user manager'а с `DefaultLimitAS=infinity` — кап ядра
обходён легально, приложение живёт в `app.slice/*.service` и переживает рестарт ядра.

Это **не ОЗУ**. Физической памяти хватало всегда (`Max resident set: unlimited`);
падение происходило на резервировании *виртуального* адресного пространства.

---

## Хронология и симптомы

### Этап 0 (до 0.1.1) — исходная беда

- `daemon_turn "Открой X"` → agent → `launch` → `{"launched": true}` — а окна нет.
- Вручную `gtk-launch firefox` из терминала — работает.
- Firefox после запуска через агента: окно crashed / «restore tabs».
- `vyn restart` убивал запущенные приложения.

Корни той эпохи (исправлены в 0.1.1–0.1.3):
- `spawn_detached` глушил stderr в `/dev/null` и не проверял exit status → любые падения
  gtk-launch выглядели как успех.
- Ядро применяло `RLIMIT_AS = 512 МБ` (дефолт) к плагину → gtk-launch падал по
  `SIGABRT: failed to map libgvfsdbus.so`.
- Ребёнок оставался в cgroup `vyn/launcher` → умирал вместе с рестартом ядра.

Фиксы 0.1.1–0.1.3: `wait_with_output` + проверка статуса; `pre_exec` с `setsid` +
`setrlimit(RLIMIT_AS=∞)`; обёртка `systemd-run --user --scope --collect`; drop-in
`max_vmem_mb: 1024`, `sandbox: false`; перепаковка в `dist/launcher/versions/0.1.3`.

### Этап 1 (эта сессия) — «почему всё ещё падает?»

Развернутый бинарник сверлен с dist 0.1.3 ✓ (zip `c02549…`, внутри `40ebc237…`),
но живой процесс плагина показывает:

```
$ grep 'address space' /proc/<launcher-pid>/limits
Max address space    1073741824    1073741824    bytes     ← soft=hard = 1 GiB
```

А в ядре (`vynkor/src/plugins/runner.rs:85-115`, комментарий AUDIT M-03):

> Applied to every spawned plugin regardless of `sandbox`

```rust
setrlimit(Resource::RLIMIT_AS, max_vmem_bytes, max_vmem_bytes) // soft И hard
```

То есть кап ставится **всем плагинам безусловно** — это осознанная политика аудита,
её менять нельзя (иначе любой плагин сможет сам снять с себя лимит).

### Доказательство механизма (воспроизведение пути плагина)

Эмулируем окружение плагина и запускаем ровно то же, что делает runner:

```
( ulimit -v 1048576; systemd-run --user --scope --collect -- gtk-launch firefox )
→ Running as unit: run-p702550-i709321.scope
→ exit=0 за 155 мс                        ← вот источник "launched:true"
→ ExceptionHandler::GenerateDump … minidump generation succeeded
                                            ← Firefox умер мгновенно, Breakpad
                                              сложил дамп в ~/.config/mozilla/
                                              firefox/*/minidumps/
→ pgrep -x firefox → пусто
```

Контроль без капа:

```
systemd-run --user --scope --collect -- gtk-launch firefox   # без ulimit
→ pid живёт, /proc/<pid>/limits: Max address space = unlimited
→ cgroup: app.slice/run-*.scope
```

Вывод: `gtk-launch` мал и выживает под капом → рапортует успех; Firefox резервирует
гигабайты VA (V8 sandbox и пр.) → mmap падает → мгновенный краш процесса.
Alacritty помещалась в 1 ГБ — поэтому «работало» и маскировало баг.

### Тупик №1: поднять лимит нельзя

Проверка прав на подъём:

```python
resource.setrlimit(RLIMIT_AS, (1024**3, 1024**3))            # как ядро
resource.setrlimit(RLIMIT_AS, (-1, -1))
→ ValueError: not allowed to raise maximum limit             # EPERM
```

Непривилегированный процесс может **опускать** hard limit и поднимать soft **до текущего
hard**, но не выше. Следствие: `pre_exec { setrlimit(AS, ∞, ∞) }` внутри плагина —
мёртвый код (и им был в fallback-ветках 0.1.3). Обойти можно только сменив *кто форкает*
процесс приложения.

## Решение: transient service вместо scope

`systemd-run --user --collect <unit>` **без** `--scope`: команду форкает PID1 user manager'а,
лименты берутся из его дефолтов (`DefaultLimitAS=infinity` — подтверждено пробой), а не от
плагина. Но у service-режима два собственных грабля — оба пойманы и закрыты.

### Грабля А: сервис не наследует env плагина

Проба содержимого `/proc/self/environ` внутри сервиса показала отсутствие
`DISPLAY`, `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS` → gtk-launch молча завершался
(`exit=0`, ни строчки вывода!), ничего не запуская. Лечение: явный проброс через
`--setenv=K=V` тех переменных, что нужны GUI-приложению:

```
DISPLAY, WAYLAND_DISPLAY, XAUTHORITY, DBUS_SESSION_BUS_ADDRESS,
XDG_SESSION_TYPE, XDG_CURRENT_DESKTOP, GDK_BACKEND
```

### Грабля Б (главная): KillMode=control-group убивает приложение

Симптоматика: прямой бинарь под сервисом **работает** (`/usr/lib/firefox/firefox` → живой
pid), а `gtk-launch`/`gio launch` — «успех», ничего не запущено. Причина: у transient unit
main pid = сам лаунчер (gtk-launch). Когда он выходит, юнит деактивируется, и дефолтный
`KillMode=control-group` **выкашивает весь cgroup юнита** — вместе с только что
запущенным Firefox. У `--scope` такой семантики нет (нет понятия main pid), поэтому
этап 0.1.3 частично работал.

Лечение: `--property=KillMode=process` — при остановке юнита убивается только main;
осиротевшее приложение продолжает жить вне супервизии (что и является целью launch).

### Деталь: пайпы и EPIPE

`--pipe` подключает stdio юнита к нашим фд, чтобы поймать stderr при ошибке. Но
`systemd-run` возвращается сразу после постановки юнита в старт (~100 мс), а приложение
живёт часами: если просто сделать `wait_with_output` и вернуть ответ, ридеры закроются,
и первое же письмо приложения в stderr получит EPIPE/SIGPIPE. Поэтому дрейн вынесен в
фоновую задачу, владеющую child до полного выхода дерева процессов, а результат
очереди приходит через oneshot; синхронное окно 5 с ловит реальные ошибки gtk-launch
(они завершаются мгновенно).

---

## Финальный дизайн (runner.rs @ 0.1.4)

```
spawn_detached(bin, args)
├─ systemd-run доступен?
│  ├─ sargs = --user --collect --property=KillMode=process
│  │        [+ --pipe если delegating]
│  │        [--setenv=K=V × присутствующие GUI-переменные]
│  ├─ delegating (gtk-launch|gio|xdg-open):
│  │     spawn(piped) → фоновая drain-задача(wait_with_output) → oneshot
│  │     ≤5 c ждём результат очереди: !success → ERR_LAUNCH_FAILED + stderr
│  │     таймаут → считаем handoff состоявшимся → Ok
│  └─ остальное (steam, kitty/alacritty/ghostty, tmux…): fire-and-forget, null stdio
└─ fallback без systemd (dev-режим): setsid + попытка setrlimit(AS→∞)
   (при закапченном плагине это EPERM — известное ограничение)
```

Результат запуска приложения: `app.slice/run-*.service`, `RLIMIT_AS=unlimited`,
вне `vyn/` → переживает `vyn restart`. Сам плагин по-прежнему закапчен ядром (1 ГБ) —
политика M-03 соблюдена: кап ограничивает плагин, но не делегированные им пользовательские
приложения.

### Верификация (сквозняк)

```
$ vyn-act launch '{"app_id":"firefox"}'
→ {"launched":true, provider:"desktop", command:"gtk-launch"}
t+8s  pid=762487 alive
       cgroup : app.slice/run-p*.service
       limits : Max address space = unlimited
t+30s pid=762487 ALIVE ✓
```
Тесты: clippy `-D warnings` чист; 37 unit + 5 e2e зелёные.
Артефакты: `dist/launcher/versions/0.1.4/launcher-0.1.4.zip` sha256 `7e72b7ee…`,
установленный бинарник `3fc4f60d…`, registry.json обновлён (unsigned, как раньше).

---

## Методика: команды, которыми доказывали

Полезно на будущее — каждая гипотеза закрывалась живым экспериментом:

```bash
# 1. какой лимит реально висит на процессе?
grep 'address space' /proc/<pid>/limits          # soft и hard раздельно!

# 2. воспроизвести путь плагина с эмуляцией капа
( ulimit -v 1048576; systemd-run --user --scope --collect -- gtk-launch firefox )
#   → exit 0 за ~150мс + Breakpad minidump = механизм найден

# 3. можно ли поднять hard limit из капа? (нет)
python3 -c "import resource as r; r.setrlimit(r.RLIMIT_AS,(1024**3,1024**3)); \
            r.setrlimit(r.RLIMIT_AS,(-1,-1))"    # ValueError/EPERM

# 4. какие лимиты/env у transient unit?
systemd-run --user --collect --unit=t-probe -- sh -c \
  "grep address /proc/self/limits; tr '\0' '\n' </proc/self/environ | grep -E '^(DISPLAY|WAYLAND)'"

# 5. KillMode-гипотеза: прямой бинарь vs gio/gtk-launch под сервисом
systemd-run --user --collect -p KillMode=process -- gtk-launch firefox   # ✓ живёт

# 6. куда ушли ошибки юнита
journalctl --user -u <unit-name> --no-pager -n 30
ls -lt ~/.config/mozilla/firefox/*/minidumps/    # следы mmap-крашей
```

Инструментальные заметки:
- `vyn-act` требует **оба** env: `VYN_JWT_TOKEN` (минт: `vyn token mint --device vyn-act
  --permissions launch`) и `VYN_JWT_SECRET` из config.yaml — иначе `ErrMacMissing` /
  «frame MAC verification failed».
- REST API ядра — **HTTPS с пиннингом серта** (`/run/user/1000/vyn-tls`); голый curl по
  http:// на порт 8888 даёт мусор «HTTP/0.9». Порт 8888 — это API/TLS, UDS лежит рядом.
- Управление жизненным циклом плагинов требует permission `PERMISSION_KERNEL_ADMIN`:
  `vyn plugin -c ~/.config/vyn/config.yaml --token "$TOK" stop|start launcher`
  (401 = нет токена, 403 = не тот permission).
- `bash/zsh ulimit -v N` опускает только **soft** — для эмуляции состояния плагина
  (soft=hard) нужен именно `setrlimit` (см. п.3).

---

## Осталось / известно и принято

- **Fallback-ветки без systemd-run**: их `setrlimit(AS→∞)` — EPERM при закапченном
  плагине (мёртвый, но безвредный код; нужен только в dev без systemd).
- `RealRunner::run` (дискавери-команды типа `tmux ls`) не тронут — короткие запросы,
  кап им не мешает.
- `vynm list` показывает launcher `0.1.0` в installed.json — косметика ledger'а,
  на исполнение не влияет.
- Пакеты по-прежнему unsigned (как все предыдущие локальные версии).
- `daemon_turn` иногда ERR_DAEMON_BUSY / таймауты 30–60 с — латентность LLM в goal loop
  агента, к пути запуска отношения не имеет. Полный голосовой цикл «Открой Firefox»
  перепроверить отдельно.

## Уроки

1. **«ОЗУ не хватает?» — сначала `/proc/<pid>/limits`**: RLIMIT_AS — про *виртуальное*
   адресное пространство; браузеры/Chromium-подобные резервируют гигабайты VA и умирают
   на mmap задолго до реленного RSS.
2. **soft≠hard**: проверять обе колонки; непривилегированный подъём hard невозможен —
   любой дизайн «спавнер сбрасывает себе лимиты» под капом нежизнеспособен.
3. **`--scope` ≠ сервис**: scope = «мой форк с бухгалтерией systemd» (наследует всё),
   service = «форк PID1» (наследует лимиты менеджера, но НЕ env).
4. **KillMode по умолчанию опасен для launch-делегирования**: выход обёртки убивает
   весь cgroup юнита вместе с полезной нагрузкой.
5. **exit code обёртки ≠ успех запуска приложения** (dbus activation, fire-and-forget,
   KillMode-sweep) — единственный честный критерий: процесс жив через N секунд и его
   cgroup там, где ожидали.
