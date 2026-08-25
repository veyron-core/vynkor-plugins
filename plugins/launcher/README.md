# launcher plugin

Launch apps and Steam games by name for vynkor plugins. Six providers:

- **desktop** — XDG `.desktop` files (`/usr/share/applications`, `~/.local/share/applications`, or `LAUNCHER_DESKTOP_DIRS`). Parses `Name`/`Exec`/`Terminal`/`Path`/`NoDisplay`/`Hidden`, strips field codes (`%f %U` etc.), launches via `gtk-launch` → `gio` → direct `Exec` argv. `Terminal=true` entries are wrapped with `LAUNCHER_TERMINAL` (tmux/kitty/alacritty/ghostty).
- **steam** — Steam games via `steam://rungameid/<appid>`. Reads `libraryfolders.vdf` + `appmanifest_*.acf` under each `LAUNCHER_STEAM_ROOTS/*/steamapps`. Launches via `$LAUNCHER_STEAM_BINARY` (default `steam`).
- **tmux** — tmux sessions (`tmux ls`). `launch` creates `new-window` (or `new-session` if `LAUNCHER_TMUX_SESSION` missing) with `-c <Path> -- <cmd>`. Also launch arbitrary commands as `tmux` provider.
- **kitty / alacritty / ghostty** — terminal emulators. `launch` wraps command as `kitty -- <cmd>`, `alacritty -e <cmd>`, `ghostty -e <cmd>`. Useful for TUI apps or `Terminal=true` desktop entries.

All spawns are argv-only — never a shell — so `app_id`/`args` cannot inject commands. `LAUNCHER_ALLOWED_IDS` optionally restricts which ids may be launched (default allow all discovered).

## Operator note

`launcher` declares `PERMISSION_LAUNCH` (`plugin.json: "permissions": ["launch"]`, per-action `permission: "launch"` on every action). Requires `sandbox: false` because it spawns host binaries and reads host desktop/Steam paths.

Launches are delegated through a transient systemd **service** (`systemd-run --user --collect
--property=KillMode=process` + GUI-env forwarding), never `--scope` and never a bare child:
the kernel caps every plugin with an unraisable `RLIMIT_AS` (AUDIT M-03), a `--scope` child
inherits it (big-VA apps like Firefox die on mmap), the default `KillMode` would kill the app
when the delegating launcher exits, and a service inherits no env. Full story and evidence:
[`docs/LAUNCHER_RLIMIT_AS_POSTMORTEM_2026-08-25.md`](../../docs/LAUNCHER_RLIMIT_AS_POSTMORTEM_2026-08-25.md).

## Actions

### `launch`

```json
{ "app_id": "firefox", "provider": "auto", "args": ["--new-window"], "dry_run": false }
```

- `app_id` — required, 1..256 chars. Matches desktop file stem (e.g. `firefox`) or Steam `appid` (e.g. `12345`); also accepts display `Name` when unique.
- `provider` — `auto` (desktop+steam), `desktop`, `steam`, `tmux`, `kitty`, `alacritty`, `ghostty`. `tmux`/`kitty`/etc. accept any `app_id` as direct command when not found in catalog.
- `args` — optional extra argv (max 32, each 1024 chars, no null bytes), appended verbatim.
- `dry_run` — when true, validates and returns what would be launched without spawning.

Response:

```json
{ "launched": true, "dry_run": false, "provider": "desktop", "app_id": "firefox", "name": "Firefox", "command": "gtk-launch", "argv": ["firefox"] }
```

Errors: `ERR_LAUNCH_NOT_FOUND`, `ERR_LAUNCH_BLOCKED`, `ERR_LAUNCH_NOT_SUPPORTED`, `ERR_LAUNCH_PROVIDER_MISSING`, `ERR_LAUNCH_SPAWN_FAILED`, `ERR_LAUNCH_BAD_PARAMS`.

### `launch_list`

```json
{ "provider": "auto", "query": "fox", "limit": 100, "include_hidden": false }
```

- `query` — case-insensitive substring over `id` and `name`.
- `limit` — 1..500 (default 100).
- `include_hidden` — include `NoDisplay`/`Hidden` entries.

Response:

```json
{ "apps": [{ "id": "firefox", "name": "Firefox", "provider": "desktop", "exec": "firefox %u", "path": "/usr/share/applications/firefox.desktop" }], "providers": ["desktop","steam"] }
```

### `launch_providers`

```json
{}
```

Response: array of `{id, name, available, description, roots}` for `desktop`/`steam`/`tmux`/`kitty`/`alacritty`/`ghostty`.

## Terminal integration

`Terminal=true` in a `.desktop` (e.g. `nvim`, `btop`) is wrapped via `LAUNCHER_TERMINAL`:

- `tmux` → `tmux new-window -d -t <session> -n <id> -c <Path> -- <Exec>` (or `new-session -d -s <session>` if session missing)
- `kitty` → `kitty --directory <Path> -- <Exec>`
- `alacritty` → `alacritty --working-directory <Path> -e <Exec>`
- `ghostty` → `ghostty --working-directory <Path> -e <Exec>`
- `none` → run `Exec` directly (may fail for TUI)

Direct terminal launches (no `.desktop` needed):

```json
{ "app_id": "htop", "provider": "tmux" }      // tmux new-window -- htop
{ "app_id": "htop", "provider": "kitty" }     // kitty -- htop
{ "app_id": "nvim", "provider": "alacritty" } // alacritty -e nvim
```

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `LAUNCHER_DESKTOP_DIRS` | `~/.local/share/applications` + `$XDG_DATA_DIRS/applications` | Comma-separated desktop dirs to scan |
| `LAUNCHER_STEAM_ROOTS` | `~/.steam/steam`, `~/.local/share/Steam` | Comma-separated Steam roots (each `…/steamapps` is scanned) |
| `LAUNCHER_ALLOWED_IDS` | *(allow all)* | Comma-separated allowlist of launchable ids; `blocked` → `ERR_LAUNCH_BLOCKED` |
| `LAUNCHER_TIMEOUT_MS` | `10000` | Per-spawn timeout (reserved; current detach is fire-and-forget) |
| `LAUNCHER_STEAM_BINARY` | `steam` | Binary for Steam launches |
| `LAUNCHER_DESKTOP_LAUNCHER` | `auto` | `auto`/`gtk-launch`/`gio`/`xdg-open`/`exec` — how desktop entries are launched |
| `LAUNCHER_TERMINAL` | `auto` | `auto`/`none`/`tmux`/`kitty`/`alacritty`/`ghostty` — terminal for `Terminal=true` entries (`auto` detects tmux→ghostty→kitty→alacritty) |
| `LAUNCHER_TMUX_SESSION` | `vynkor` | tmux session name for `tmux` provider and `Terminal=true` wrapping |
| `LAUNCHER_CACHE_TTL_MS` | `60000` | In-memory scan cache TTL in ms (`0` disables). Cached per-provider (desktop/steam), `include_hidden` filtered from full cache. |

## Security

- argv-only spawn, no shell.
- Field codes in `Exec` are stripped before argv construction.
- Allowlist (`LAUNCHER_ALLOWED_IDS`) gates `launch` even when the app is discovered.
- `args` size-capped and null-byte rejected.
- No path traversal: scan is confined to configured roots; ids are stems, not paths.

## Testing

`cargo test -p launcher-plugin --manifest-path plugins/launcher/Cargo.toml` — 35 unit + 5 e2e, no network/kernel, uses tempdirs and fake launcher.
