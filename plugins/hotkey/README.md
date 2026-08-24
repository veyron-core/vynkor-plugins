# hotkey plugin

Global key combos → kernel events. Hold a key, talk, release — the
[`daemon`](../daemon/) turns that into a voice turn. The same events drive
anything else an operator wants wired to the keyboard (mute toggles,
"stop talking", scene switches).

Two backends behind one identical action/event surface:

| Backend | What it is | Press-and-hold | Runtime bind |
|---|---|---|---|
| `portal` | XDG GlobalShortcuts (`org.freedesktop.portal.GlobalShortcuts`) over the session D-Bus — the Wayland-native way; the compositor/DE owns the grab and shows the shortcut in its own UI | ✅ `Activated`/`Deactivated` map exactly to key down/up | ✅ `hotkey_bind` re-binds live |
| `manual` | No OS integration. Events arrive via `hotkey_inject`, which any external wiring can call — compositor exec binds (`hyprctl bind`-style), `curl` against the kernel REST API, tests | depends on the wiring | n/a |

Backend selection (`HOTKEY_PLUGIN_BACKEND`): `auto` (default) tries the
portal and degrades to manual with a log line when no desktop portal
answers (headless boxes, CI); `portal` demands it; `manual` never opens
D-Bus.

## Events

Both best-effort, published under the kernel's namespace:

- `plugin.hotkey.hotkey_pressed` — payload `{"binding": "<id>"}`
- `plugin.hotkey.hotkey_released` — payload `{"binding": "<id>"}`

The payload shape is a cross-plugin contract: the daemon filters its
push-to-talk binding by `payload["binding"]`. Portal signals and
`hotkey_inject` produce byte-identical events.

Trigger grammar: `MOD+MOD+KEY` — modifiers `Ctrl`/`Control`,
`Super`/`Meta`/`Logo`/`Win`, `Alt`, `Shift`; one non-modifier key from
letters/digits, `F1`–`F35`, `space`/`enter`/`escape`/`tab`, arrows and
navigation, XF86 media names (`VolumeUp`, `VolumeMute`, `MediaPlayPause`,
…), or punctuation. At least one modifier is required — bare-letter global
binds would fire on every keystroke in every app. Triggers normalize to
the portal's spelling at bind time (`Ctrl+Shift+Space` →
`CTRL+SHIFT+space`); unknown keys fail loudly at bind time, never as
silently-dead shortcuts at press time.

## Actions

| Action | Params | Result |
|---|---|---|
| `hotkey_bind` | `{id, trigger, description?}` | `{bound: true, id, trigger, backend}` — registers/replaces a binding; in portal mode the full set is re-bound to the desktop live |
| `hotkey_unbind` | `{id}` | `{unbound, id}` — idempotent: already-absent id → `{unbound: false}`, not an error |
| `hotkey_list` | `{}` | `{backend, count, bindings: [{id, trigger, description}]}` |
| `hotkey_status` | `{}` | `{backend, count}` — lean variant for dashboards |
| `hotkey_inject` | `{binding, state: pressed\|released}` | publishes the matching event immediately — the manual backend's event source |

Bind ids are `[a-z0-9_.-]{1,64}` and are the stable handle every consumer
keys on; the daemon's `DAEMON_PLUGIN_PTT_BINDING` must equal a binding id
here (default `"ptt"`).

## Push-to-talk wiring (daemon)

1. Bind once at boot:
   `HOTKEY_PLUGIN_BINDINGS=ptt=Ctrl+Shift+Space`
2. Point the daemon at it:
   `DAEMON_PLUGIN_MODE=ptt` + `DAEMON_PLUGIN_ENABLE`d.
3. Hold the combo → daemon opens the mic; release → transcript → agent →
   spoken answer. A stuck key auto-releases after
   `DAEMON_PLUGIN_PTT_MAX_HOLD_MS`.

Without the hotkey plugin registered, ptt mode simply never fires (the
daemon stays idle) — window/vad modes don't need it.

## Compositor wiring without the portal (Hyprland example)

```
# .config/hypr/hyprland.conf
bind = SUPER, X, exec, vynm action-call hotkey hotkey_inject '{"binding":"ptt","state":"pressed"}'
bindl = SUPER, X, release, exec, vynm action-call hotkey hotkey_inject '{"binding":"ptt","state":"released"}'
```

(or the equivalent `curl -X POST` against the kernel REST API with an
operator JWT). Each exec lands as `hotkey_inject` and produces the same
events the portal backend would.

## Permissions

`PERMISSION_SYSTEM` — global input interception is a system-level
capability even though the portal mediates it; also held for
`hotkey_inject`, which lets a caller synthesize trusted input events.
`PERMISSION_EVENT_PUBLISH` — the pressed/released events. A dedicated
`PERMISSION_HOTKEY` lands with the next wire enum bump (root ROADMAP keeps
the note).

No evdev reading anywhere — raw keyboard sniffing is a keylogger surface
and breaks the narrow-permission-per-plugin model.

## Testing

`cargo test` — 12 unit tests (trigger normalization table, env binding
parsing, store replace/remove semantics, request validation) plus 4 e2e
tests over a fake kernel socket pair (registration handshake asserting
permissions/actions, inject → pressed/released events in order with the
daemon-contract payload shape, bind/list/unbind/status roundtrip including
the idempotent unbind, boot-binding env loading and bad-spec degradation).
The portal D-Bus path needs a real desktop session and is exercised
manually; its pure helpers (request-path escaping) are unit-tested.

## Status

v0.1. Manual backend is complete; the portal backend targets
`xdg-desktop-portal` ≥ 1.7 with GlobalShortcuts (Hyprland, GNOME, KDE,
wlroots portals). X11 `XGrabKey` fallback remains future work per root
ROADMAP — X11 sessions can use compositor/inject wiring meanwhile.
