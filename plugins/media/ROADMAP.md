# media plugin roadmap

Local MPRIS playback control — one blessed path for `play/pause/seek/volume/status/list` over the session D-Bus.

## v1 — shipped (0.1.0, local-only, no permissions)

10 actions behind `org.mpris.MediaPlayer2.*`:

- `media_list_players` — `ListNames` filtered `org.mpris.MediaPlayer2.*`, sorted, allowlist `MEDIA_PLUGIN_PLAYERS`
- `media_status {player?}` — `PlaybackStatus + Metadata + Volume + Position` via `Properties.Get`
- `media_play {player?, uri?}` — `OpenUri?` + `Play`
- `media_pause / media_play_pause / media_next / media_prev / media_stop` — direct `Player.*`
- `media_seek {position_ms, player?}` — `Seek(offset)` where `offset = target - Position`
- `media_volume {level, player?}` — `Properties.Set Volume` (0.0-1.0, accepts 0-100 int% via `main.rs:parse_volume`)

`zbus 4` async, `RealBackend` + `MockBackend`, 8 unit tests (parsing + mock), `permissions: []` (pure local IPC).

## v0.0.2 — shipped (bugfix + rate/shuffle/loop, 12 actions) — was 0.2.0, re-tagged as 0.0.2 per request

Fixes from `BUGS.md` verified on `firefox.instance_1_424` + `mpd` + `TelegramDesktop` (2026-08-20):

- `seek` now `SetPosition(trackId, pos_us)` primary (ObjectPath validated), `Seek(delta)` fallback only on `NotSupported/UnknownMethod`; overflow guard `checked_mul(1000)`, `NoTrack` → `ERR_MEDIA_NO_TRACK` (fixes TECH-3, part of BUG-1).
- `status` no longer swallows `PLAYER_VANISHED` — `PlaybackStatus/Volume/Position` propagate `ERR_MEDIA_PLAYER_VANISHED`, `Rate/Shuffle/LoopStatus` best-effort, `Rate` defaults to `1.0` while Playing. Adds `rate/shuffle/loop_status` to output (additive).
- `play_pause` race fixed via 50/100/150ms poll (BUG-3).
- `parse_volume` int vs float disambiguated (`is_u64/is_i64` before `as_f64`); `player`/`uri` param validation now `ERR_MEDIA_BAD_PARAMS` instead of silent fallback.
- `parse_metadata` length accepts `i64/u64/i32/u32/i16/u16/u8` (was only i64/u64).
- `metadata` cache per player (`OnceLock<Mutex<HashMap>>`) merges missing `length/title/track_id` on sparse Firefox updates (BUG-4).
- `Rate` extrapolation in `status`: if `Position==0 && Playing && rate!=0 && cached_pos>0` → `cached_pos + elapsed*rate` (partial BUG-2).
- Taxonomy unified: all D-Bus errors now `ERR_MEDIA_BUS_UNAVAILABLE / PLAYER_VANISHED / SEEK_FAILED / NOT_SUPPORTED / BAD_PARAMS`.
- 14 tests (was 8) incl. `seek_no_track`, `seek_set_position`, `status_shuffle_loop_rate`, `metadata_cache`.

Remaining gaps → see `BUGS.md` (`BUG-1 Firefox seek still high`, `BUG-2 stale Position full fix needs PropertiesChanged stream`).

## v0.0.3 — shipped (v1.1 polish, 13 actions) — 2026-08-21

- **Capability guards**: `CanPlay`/`CanPause`/`CanPlayPause`/`CanGoNext`/`CanGoPrevious`/`CanSeek`/`CanControl` checked before the call — explicit `false` → `ERR_MEDIA_NOT_SUPPORTED`; missing property never blocks (minimal players keep working).
- **Error reclassification**: `classify_dbus` — `No such property` etc. now `ERR_MEDIA_NOT_SUPPORTED`, no longer misreported as `PLAYER_VANISHED` (the firefox Shuffle case).
- **Signal watcher (full BUG-2 fix for compliant players)**: per-player background task subscribes `PropertiesChanged` + `Seeked(int64)` and feeds `POS_CACHE` `(pos, rate, updated_at)`; `extrapolate_position` trusts samples only within a 120s window (MPRIS needs no periodic Position updates). Watcher writes static caches only — single-reader rule on `VynkorClient` untouched.
- **`media_seek_relative {offset_ms}`** — signed offset, clamps at 0, shares the absolute-seek core (`seek_absolute_on`) with `media_seek`.
- **Negative tests**: empty/mixed/wrong-typed `xesam:artist`, `mpris:length` small-int variants + float/negative rejection, all guard paths, classification unit tests. 42 tests (was 14).

Deferred from v1.1: publishing `media.state_changed` to the kernel event bus. It
needs `PERMISSION_EVENT_PUBLISH` plus an outbound path from the watcher task;
the SDK's sequential serve loop owns `VynkorClient` exclusively (single-reader
rule, `docs/PLUGIN_AUTHORING.md` §1), so events wait until the loop migration.

## v1.2 — loop migration + MPD/mpv hardening

- Migrate to the calendar-style single-reader select loop (RPC proxy + outbound channel); then publish `media.state_changed` opt-in behind declared `PERMISSION_EVENT_PUBLISH`.
- MPD `NoTrack` handling done (0.2.0). Next: `media_queue`/`media_playlist` (TrackList `GetTracksMetadata` + `AddTrack`/`RemoveTrack`/`GoTo`) for MPD, gated behind `MEDIA_PLUGIN_ENABLE_TRACKLIST=false` default (browsers don't implement TrackList).

## v2 — remote providers (requires `network` + `secrets`)

- `media_search {query, limit}` + `media_play {uri}` for Spotify/YouTube Music via `network.http_request`, keys from `secrets` vault (`SPOTIFY_ACCESS_TOKEN`, `YOUTUBE_API_KEY` via `SECRETS_PLUGIN_MASTER_KEY`). Adds `PERMISSION_NETWORK` + `PERMISSION_SECRETS` (caller of gated actions).
- System volume (`pactl`/`wpctl`) via `PERMISSION_SYSTEM` optional mode — currently `media_volume` only touches MPRIS player volume, not host mixer.
- `config_schema` additions: `MEDIA_PLUGIN_SECRETS_ALLOWLIST`, `MEDIA_PLUGIN_SPOTIFY_MARKET` etc.

## Non-goals

- No audio streaming — `tts_speak` already streams Opus `AudioStreamChunk`s; `media` is control-plane only.
- No window focus/raise — `window` plugin will handle `Raise`.
- No new `PermissionType` enum value — local MPRIS needs none; remote mode reuses `network`/`secrets`.

## References

- MPRIS 2.2: https://specifications.freedesktop.org/mpris-spec/latest/
- zbus 4.4: `Connection::session`, `DBusProxy::list_names`, `PropertiesProxy::get/set`
