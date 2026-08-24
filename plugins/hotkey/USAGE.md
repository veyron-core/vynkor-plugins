# hotkey — caller guide

Every request/response below is `ActionRequest.params_json` /
`ActionResponse.data_json`. Errors are `ACTION_ERROR` with a
human-readable message naming the offending field.

## hotkey_bind

```json
{ "id": "ptt", "trigger": "Ctrl+Shift+Space", "description": "Push to talk" }
```

- `id` — required, `[a-z0-9_.-]{1,64}`. The stable handle consumers
  subscribe by. Re-binding an existing id replaces its trigger/description
  in place (order in `hotkey_list` stays stable).
- `trigger` — required, ≤ 64 chars, operator spelling:
  - modifiers: `Ctrl`/`Control`, `Super`/`Meta`/`Logo`/`Win`, `Alt`,
    `Shift` (case-insensitive; duplicates collapse)
  - exactly one non-modifier key: letters/digits, `F1`–`F35`, `space`,
    `enter`, `escape`, `tab`, `backspace`, `delete`, arrows/home/end/page
    keys, `VolumeUp`/`VolumeDown`/`VolumeMute`/`Media*` XF86 names,
    punctuation (`!@#$%^&*()[]{}...`)
  - ≥ 1 modifier mandatory (bare-letter global binds fire on every
    keystroke everywhere)
- `description` — optional, ≤ 200 chars, default `"hotkey <id>"`; shown in
  the desktop's shortcut UI and `hotkey_list`.

Response:

```json
{ "bound": true, "id": "ptt", "trigger": "CTRL+SHIFT+space", "backend": "portal" }
```

`backend: "manual"` means the binding is stored for inject/compositor use
only — no OS grab exists.

Errors: field validation (`invalid binding id …`, `invalid trigger: …`
naming why), portal denial (`shortcuts were denied by the desktop
(response code N)`), portal timeouts.

## hotkey_unbind

```json
{ "id": "ptt" }
```

Response: `{ "unbound": true, "id": "ptt" }` — or `{ "unbound": false }`
when the id was already absent (idempotent, never an error).

## hotkey_list / hotkey_status

```json
{}
```

```json
{
  "backend": "portal",
  "count": 2,
  "bindings": [
    { "id": "ptt", "trigger": "CTRL+SHIFT+space", "description": "hotkey ptt" },
    { "id": "mute", "trigger": "LOGO+m", "description": "hotkey mute" }
  ]
}
```

`hotkey_status` returns the same shape minus `bindings`.

## hotkey_inject

```json
{ "binding": "ptt", "state": "pressed" }
```

Publishes `hotkey_pressed`/`hotkey_released` immediately and returns:

```json
{ "published": true, "binding": "ptt", "state": "pressed" }
```

`binding` need not be registered — inject is the manual backend's event
source. This is the action compositor exec binds / REST callers hit.

## Events

| Namespaced type | Payload | When |
|---|---|---|
| `plugin.hotkey.hotkey_pressed` | `{"binding": "<id>"}` | combo pressed (portal `Activated`) or inject |
| `plugin.hotkey.hotkey_released` | `{"binding": "<id>"}` | combo released (portal `Deactivated`) or inject |

Subscribe to both if you care about hold duration (the daemon does);
subscribe to `*_released` alone if you only want "user finished".

## Every error a caller can hit

| Message fragment | Meaning |
|---|---|
| `missing required field '<f>'` | self-explanatory |
| `<field> must be a string…` | wrong JSON type |
| `invalid binding id '…'` | id charset/length violated |
| `invalid trigger: trigger '…' needs one non-modifier key` | e.g. `Ctrl` alone |
| `invalid trigger: trigger 'Space' needs at least one modifier` | bare key |
| `invalid trigger: trigger '…' has more than one non-modifier key` | `Ctrl+A+B` |
| `invalid trigger: unsupported key '…'` | not in the key table |
| `shortcuts were denied by the desktop (response code N)` | user/compositor refused the bind |
| `timed out waiting for the portal rebind` | desktop portal stalled (>20 s) |
| `portal backend stopped` | worker died mid-rebind (D-Bus gone) |

## Common patterns

**Daemon push-to-talk** (env of both plugins):

```
HOTKEY_PLUGIN_BINDINGS=ptt=Ctrl+Shift+Space
DAEMON_PLUGIN_MODE=ptt
```

**Mute toggle** — consumers get two events per press; pair them by
ordering: subscribe to both types, treat `pressed`+`released` with < 500 ms
gap as a "tap".

**Compositor without portal** — see README §"Compositor wiring"; each exec
is just a `hotkey_inject` call.
