# launcher plugin — usage guide

Reference for plugin authors calling the `launcher` plugin through the kernel.
For configuration, architecture and provider details, see [`README.md`](./README.md);
for what's deferred, see [`ROADMAP.md`](./ROADMAP.md).

## The model in one minute

- **Six providers, one permission.** `launch` is gated by `PERMISSION_LAUNCH` (`launch`). Every action carries per-action `permission: "launch"` — caller must hold it (T-19 anti-laundering, same as `ai` → `network`).
- **Desktop:** scans `LAUNCHER_DESKTOP_DIRS` for `*.desktop` (XDG). Parses `Name`/`Exec`/`Terminal`/`Path`/`NoDisplay`/`Hidden`/`Type`, strips `%f %U` field codes, launches via `gtk-launch` → `gio` → `xdg-open` → `Exec` argv. `Terminal=true` entries are wrapped with `LAUNCHER_TERMINAL` (tmux/kitty/alacritty/ghostty) and `Path` as working dir.
- **Steam:** scans `LAUNCHER_STEAM_ROOTS/*/steamapps` for `appmanifest_*.acf` + `libraryfolders.vdf` additional libs, launches via `steam steam://rungameid/<appid>`.
- **Terminal emulators:** `tmux` (sessions via `tmux ls`), `kitty` (`kitty --`), `alacritty` (`alacritty -e`), `ghostty` (`ghostty -e`). Accept any `app_id` as direct command when not in catalog (e.g. `htop`).
- **Cache:** in-memory per-provider scan cache, `LAUNCHER_CACHE_TTL_MS` default 60000 (0 disables). `include_hidden` filtered from full cache, no disk.
- **All spawns argv-only** — never a shell, field codes stripped, `args` appended verbatim.

## How a call looks

`ActionRequest { action, params_json }` → `ActionResponse`:

- success → `ACTION_OK`, `data_json` = JSON below
- not found → `ACTION_NOT_FOUND`, `error` = `ERR_LAUNCH_NOT_FOUND: ...`
- failure → `ACTION_ERROR`, `error` = plain text (see [Errors](#errors))

Examples show **params** and **result**.

## Actions

### `launch` — launch an app/game/command

```jsonc
// params — desktop
{"app_id":"firefox","provider":"desktop","args":["--new-window"],"dry_run":false}
{"app_id":"Alacritty","provider":"auto","dry_run":true}
// params — steam
{"app_id":"413150","provider":"steam"}
{"app_id":"Stardew Valley","provider":"steam","dry_run":true} // name when unique
// params — terminal (direct command)
{"app_id":"htop","provider":"tmux"}
{"app_id":"nvim","provider":"kitty","args":["/tmp/file"]}
{"app_id":"btop","provider":"alacritty"}
{"app_id":"htop","provider":"ghostty","args":["--sort-key","cpu"]}
// params — desktop Terminal=true wrapping (LAUNCHER_TERMINAL=tmux)
{"app_id":"termapp","provider":"desktop"} // TermApp.desktop Terminal=true → tmux new-window -- nvim
// result
{"launched":true,"dry_run":false,"provider":"desktop","app_id":"firefox","name":"Firefox","command":"gtk-launch","argv":["firefox"]}
{"launched":false,"dry_run":true,"provider":"tmux","app_id":"htop","name":"htop","command":"tmux","argv":["new-window","-d","-t","vynkor","-n","htop","--","htop"]}
```

- `app_id` `1..256` chars, non-empty, trimmed. Matches desktop file stem (`firefox` → `firefox.desktop`) or Steam `appid` (`413150`) or display `Name` when unique (case-insensitive for `Name`). For `tmux`/`kitty`/`alacritty`/`ghostty` any string is accepted as direct command when not found in catalog.
- `provider` `auto` (default, desktop+steam) | `desktop` | `steam` | `tmux` | `kitty` | `alacritty` | `ghostty`. `auto` does not include terminal emulators.
- `args` `0..32` items, each `0..1024` chars, no `\0`. Appended after resolved argv (never shell-interpreted).
- `dry_run` default `false` — when true, validates, resolves `command`/`argv` and returns without spawning (useful for `agent` planning).
- `Terminal=true` desktop entries: when `app.terminal` and `LAUNCHER_TERMINAL != none`, `Exec` is wrapped:
  - `tmux` → `tmux new-window -d -t <session> -n <id> -c <Path> -- <Exec>` (or `new-session -d -s <session>` if session missing, checked via `tmux has-session`)
  - `kitty` → `kitty --directory <Path> -- <Exec>`
  - `alacritty` → `alacritty --working-directory <Path> -e <Exec>`
  - `ghostty` → `ghostty --working-directory <Path> -e <Exec>`
- Success returns `command` (binary) + `argv` (full argv that was/would be spawned) + `provider` that handled it.

### `launch_list` — list available apps

```jsonc
// params
{"provider":"desktop","query":"fox","limit":10,"include_hidden":false}
{"provider":"steam","limit":5}
{"provider":"tmux"} // sessions
{"provider":"auto","query":"Alacritty"}
{"query":"code","limit":20} // auto = desktop+steam
// result
{"apps":[{"id":"firefox","name":"Firefox","provider":"desktop","exec":"firefox %u","path":"/usr/share/applications/firefox.desktop","hidden":false,"terminal":false,"working_dir":null}],"providers":["desktop"]}
{"apps":[{"id":"413150","name":"Stardew Valley","provider":"steam","exec":null,"path":"/home/behzod/.steam/steam/steamapps/appmanifest_413150.acf","hidden":false,"terminal":false,"working_dir":null}],"providers":["steam"]}
{"apps":[{"id":"vynkor","name":"vynkor","provider":"tmux","exec":null,"path":"tmux:vynkor","hidden":false,"terminal":false,"working_dir":null}],"providers":["tmux"]}
```

- `provider` default `auto` (desktop+steam). `tmux` returns `tmux ls` sessions, `kitty`/`alacritty`/`ghostty` return `[]` (no catalog).
- `query` case-insensitive substring over `id` and `name`, `1..256`, `limit` default `100`, `1..500`.
- `include_hidden` default `false` — when false, `NoDisplay`/`Hidden` entries are filtered from cache.
- Sorted by `id`, truncated to `limit`. Uses in-memory cache (`LAUNCHER_CACHE_TTL_MS`) — `include_hidden=true` cache is full, `false` is filtered view.

### `launch_providers` — list providers and availability

```jsonc
// params
{}
// result
[
  {"id":"desktop","name":"desktop","available":true,"description":"XDG desktop files (.desktop) via gtk-launch/gio/xdg-open or direct Exec","roots":["/home/behzod/.local/share/applications","/usr/share/applications"]},
  {"id":"steam","name":"steam","available":true,"description":"Steam games via steam://rungameid/<appid> (reads libraryfolders.vdf + appmanifest_*.acf)","roots":["/home/behzod/.steam/steam"]},
  {"id":"tmux","name":"tmux","available":true,"description":"tmux sessions (tmux ls) — launch creates new-window in LAUNCHER_TMUX_SESSION","roots":["vynkor"]},
  {"id":"kitty","name":"kitty","available":true,"description":"kitty terminal — launch wraps command as kitty -- <cmd>","roots":[]},
  {"id":"alacritty","name":"alacritty","available":true,"description":"alacritty terminal — launch wraps command as alacritty -e <cmd>","roots":[]},
  {"id":"ghostty","name":"ghostty","available":true,"description":"ghostty terminal — launch wraps command as ghostty -e <cmd>","roots":[]}
]
```

- `available` → `binary_in_path` or `steamapps` dir exists. `desktop` always `true` (fallback to `Exec`).
- Takes no params — non-empty object → `ERR_LAUNCH_BAD_PARAMS`.

## Terminal integration

When a `.desktop` has `Terminal=true` and `Path=/tmp`:

```ini
[Desktop Entry]
Name=TermApp
Exec=nvim %F
Terminal=true
Path=/tmp
Type=Application
```

With `LAUNCHER_TERMINAL=tmux` (or `auto` detecting `tmux`):

```json
{"app_id":"termapp","dry_run":true}
// → {"command":"tmux","argv":["new-window","-d","-t","vynkor","-n","termapp","-c","/tmp","--","nvim"]}
```

With `LAUNCHER_TERMINAL=kitty`:

```json
// → {"command":"kitty","argv":["--directory","/tmp","--","nvim"]}
```

Direct terminal launch needs no `.desktop`:

```json
{"app_id":"htop","provider":"tmux"} // → tmux new-window -- htop
{"app_id":"btop","provider":"alacritty","args":["--utf-force"]} // → alacritty -e btop --utf-force
```

`LAUNCHER_TERMINAL=auto` detection order: `tmux` → `ghostty` → `kitty` → `alacritty` → `none`.

## Errors

All failures are `ACTION_ERROR` except unknown app → `ACTION_NOT_FOUND`:

- `ERR_LAUNCH_BAD_PARAMS: ...` — `app_id` empty/too long, `provider` invalid, `args` too many/long/null byte, `query` too long, `limit` 1..500, `launch_providers` takes no params, invalid `LAUNCHER_TERMINAL`/`LAUNCHER_DESKTOP_LAUNCHER`
- `ERR_LAUNCH_NOT_FOUND: app 'X' not found (provider=Y)` — `ACTION_NOT_FOUND`
- `ERR_LAUNCH_BLOCKED: app_id 'X' not in LAUNCHER_ALLOWED_IDS` — allowlist gate
- `ERR_LAUNCH_PROVIDER_MISSING: binary 'X' not found on PATH` — `steam`/`tmux`/`kitty`/etc. missing
- `ERR_LAUNCH_NOT_SUPPORTED: ...` — desktop entry has no `Exec`, or unknown provider
- `ERR_LAUNCH_SPAWN_FAILED: ...` — `Exec` produced empty argv, or `spawn` failed, or `tmux`/`kitty` exited non-zero

`limit`/`query` errors still surface as `ERR_LAUNCH_BAD_PARAMS`, not silent truncation.

## Config

See `config.example.yaml`. All env vars are read at startup from kernel `config.yaml` `env:`.

| Env var | Default | Meaning |
|---|---|---|
| `LAUNCHER_DESKTOP_DIRS` | `~/.local/share/applications` + `$XDG_DATA_DIRS/applications` | Comma-separated desktop dirs to scan |
| `LAUNCHER_STEAM_ROOTS` | `~/.steam/steam`, `~/.local/share/Steam` | Comma-separated Steam roots (each `…/steamapps` is scanned, `libraryfolders.vdf` adds libs) |
| `LAUNCHER_ALLOWED_IDS` | — (allow all) | Comma-separated allowlist of launchable ids |
| `LAUNCHER_TIMEOUT_MS` | `10000` | Per-spawn timeout (reserved; current detach is fire-and-forget) |
| `LAUNCHER_STEAM_BINARY` | `steam` | Binary for Steam launches |
| `LAUNCHER_DESKTOP_LAUNCHER` | `auto` | `auto`/`gtk-launch`/`gio`/`xdg-open`/`exec` |
| `LAUNCHER_TERMINAL` | `auto` | `auto`/`none`/`tmux`/`kitty`/`alacritty`/`ghostty` — terminal for `Terminal=true` |
| `LAUNCHER_TMUX_SESSION` | `vynkor` | tmux session for `tmux` provider and wrapping |
| `LAUNCHER_CACHE_TTL_MS` | `60000` | In-memory scan cache TTL, `0` disables |

`LAUNCHER_DESKTOP_DIRS`/`STEAM_ROOTS`: empty string → scan nothing; unset → defaults above. `LAUNCHER_ALLOWED_IDS`: empty/unset → allow all. `XDG_DATA_DIRS` default `/usr/local/share:/usr/share` when unset.

## Security

- argv-only spawn, never `sh -c`. Field codes (`%f %U` etc.) stripped before `exec_argv`.
- `Terminal=true` wrapping is still argv-only (`tmux new-window -- <cmd>`), no shell.
- Allowlist gates `launch` even when app is discovered.
- `args` size-capped and null-byte rejected.
- Scan confined to configured roots; `app_id` is stem (no `/` or `..` traversal — `libraryfolders.vdf` values must be absolute paths starting with `/`).

## Testing

`cargo test -p launcher-plugin` — 35 lib + 5 e2e (`UnixStream::pair`, handshake, `ACTION_OK`/`ACTION_NOT_FOUND`/`ACTION_ERROR` over wire, terminal wrapping) = 40 tests, no network/kernel, uses tempdirs and `FakeLauncher`. Live host test needs real `XDG_DATA_DIRS`/`steamapps` and `gtk-launch`/`tmux` on `PATH`.

Live smoke (requires `sandbox: false`):

```bash
cargo run -p launcher-plugin --bin launcher & # via kernel
# or direct handle_action against live host:
# launch_list desktop → lists /usr/share/applications
# launch Stardew Valley dry_run → {command:"steam",argv:["steam://rungameid/413150"]}
# launch htop provider tmux dry_run → {command:"tmux",argv:["new-window",...,"--","htop"]}
```

See `README.md` for architecture and `ROADMAP.md` for next (Categories, Icon, Flatpak if needed).
