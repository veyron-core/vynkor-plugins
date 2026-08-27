# library plugin

Local media library index — scans allow-listed roots and serves search/random/recent queries.

Thin schema over `database` (like `notes`/`contacts`): entries are `library:<id>` JSON docs, no embedded SQLite. Scanning is `WalkDir` + `mtime`/`size` cache, fully offline.

## Actions

| Action | Params | Result |
|---|---|---|
| `library_scan` | `{roots?[], force?:bool}` | `{scanned, indexed, removed, total}` |
| `library_search` | `{query?, kind?("audio"/"photo"/"video"/"all"), limit?, offset?}` | `{results: [entry...], total}` |
| `library_get` | `{id}` | `{found, entry?}` |
| `library_random` | `{kind?, count?1..20}` | `{results: [entry...]}` |
| `library_recent` | `{kind?, limit?1..100}` | `{results: [entry...]}` |
| `library_stats` | — | `{total, by_kind: {audio,photo,video,other}, last_scan_ms}` |

Entry:
```json
{"id":"1","path":"/music/a.flac","name":"a.flac","kind":"audio","ext":"flac","size_bytes":1234,"mtime_ms":1710000000000,"indexed_at_ms":1710000001000}
```
Kind mapping (ext → kind): audio `mp3/flac/wav/ogg/m4a/aac/wma/opus`, photo `jpg/jpeg/png/gif/webp/bmp/tiff/heic`, video `mp4/mkv/avi/mov/webm/m4v`, otherwise `other` (stored but filtered out by `kind` queries).

Scanning:
- Roots default to `LIBRARY_PLUGIN_ALLOWED_ROOTS` (comma-separated absolute paths). If `roots` is passed in the action, it overrides the env for that call only.
- `force=false` (default) skips files whose `size+mtime` are unchanged — re-index is mtime-guarded.
- Hard cap `LIBRARY_PLUGIN_MAX_SCAN_FILES` (default 10000) — prevents runaway walks on `/`.
- Stale entries whose file no longer exists are removed and counted as `removed`. Publishes `library_indexed {scanned,indexed,total}` after each scan.

Permissions: `PERMISSION_STORAGE` (the `library:<id>` docs) + `PERMISSION_FILES_READ` (the `library_scan` walk). The search/get/random paths are storage-only and need no extra grant.

## Config

| Env | Default | Meaning |
|---|---|---|
| `LIBRARY_PLUGIN_ALLOWED_ROOTS` | _(empty)_ | Comma-separated absolute roots. Empty → scan disabled (returns `ERR_LIBRARY_NO_ROOTS`). Example: `/home/user/Music,/home/user/Pictures` |
| `LIBRARY_PLUGIN_MAX_SCAN_FILES` | `10000` | Hard cap on files visited per scan |
| `LIBRARY_PLUGIN_DB_TIMEOUT_MS` | `5000` | `database` IPC timeout |

## Examples

```json
// first scan
{"roots": ["/home/user/Music"]} → {"scanned": 3420, "indexed": 3420, "removed": 0, "total": 3420}

// search
{"query": "beatles", "kind": "audio", "limit": 5} → {"results": [{...}], "total": 12}

// random play
{"kind": "photo", "count": 3} → {"results": [{...},{...},{...}]}
```

## Testing

`cargo test` — 5 lib unit tests (`kind_detection`, `scan_temp_dir`, search defaults) + 3 fake-kernel e2e over `UnixStream::pair` (`scan_empty_roots_error`, `stats_empty`, `search_and_get`). No filesystem side-effects outside `tempfile` dirs.
