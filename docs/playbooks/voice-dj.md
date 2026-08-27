# PLAY-04: Voice DJ — "play something for work"

`ai` picks → `media` plays (MPRIS) or `sound_play` local file.

## What it does

- `ai` (or `agent`) picks a playlist by time/context.
- `media_play` (MPRIS) for Spotify/mpd/browser, or `sound_play` for local file.

## Simple variant (without agent, via `ai` directly)

```json
{
  "action": "chat_completion",
  "params": {
    "provider": "openai",
    "base_url": "http://localhost:11434/v1",
    "model": "qwen3:8b",
    "messages": [{"role": "user", "content": "Pick a work playlist: lofi, jazz, classical. Reply with one word."}]
  }
}
// → "lofi"
{"action": "media_play", "params": {"player": "spotify"}}
// or
{"action": "sound_play", "params": {"file": "/home/user/music/lofi.mp3"}}
```

## Via `agent` (recommended)

```json
{
  "action": "goal_start",
  "params": {"goal": "play something for work, pick by my taste and time of day, play via media or sound"}
}
```

Agent: `ai` → decision → `media`/`sound` + `library` search (CAP-09) `lib_search {"query": "lofi"}` → `sound_play`.

## With library (CAP-09)

```json
{"action": "lib_search", "params": {"query": "work"}}
→ {"results": [{"path": "/music/lofi/01.mp3", "title": "Lofi Beats"}]}
{"action": "sound_play", "params": {"file": "/music/lofi/01.mp3"}}
{"action": "media_shuffle", "params": {"enabled": true}}
```

## Via `vyn ask`

```bash
vyn ask "play something for work"
vyn ask "play jazz for focus"
```

## Notes

- `media` controls foreign players (MPRIS), `sound` is the only speaker owner (local files).
- Shuffle/loop via `media_shuffle`/`media_loop` for MPRIS, for `sound` — `sound_play` each track separately.
- For random: `lib_random` (library) + `sound_play`.
