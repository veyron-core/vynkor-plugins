# vector-db plugin

Embedding upsert + similarity search for Vynkor plugins, gated by `PERMISSION_STORAGE`.
One SQLite file per calling plugin — callers cannot see each other's collections.
Brute-force cosine on L2-normalized vectors, per-collection dimension enforcement.

`v0.1` uses deterministic fake embeddings (hash-based, offline, no model files) so the
plugin is fully offline and has no native deps. `v0.2` will add `fastembed` (ONNX
`bge-small-en-v1.5` / `all-MiniLM-L6-v2`) + cloud embeddings via `network`+`secrets`.

## Actions

| Action | Params | Result |
|---|---|---|
| `vec_upsert` | `{collection, id, text?, vector?, metadata?}` | `{ok, id, dim}` |
| `vec_query` | `{collection, text?, vector?, top_k?, include_vector?, filter?}` | `{results: [{id, score, text, metadata}]}` |
| `vec_get` | `{collection, id}` | `{found, id?, text?, vector?, metadata?, dim?}` |
| `vec_delete` | `{collection, id}` | `{deleted}` |
| `vec_list` | `{prefix?}` | `{collections: [...]}` |
| `vec_stats` | `{collection}` | `{count, dim}` |

- `vec_upsert` requires at least one of `text` or `vector`. If `vector` is given it is L2-normalized and must match the collection's existing `dim` (first write defines `dim`). If only `text` is given, a fake embedding of `default_dim` is computed.
- `vec_query` requires at least one of `text` or `vector`. `top_k` default 5, cap 100. `filter` is exact match on top-level metadata keys.
- Scores are cosine similarity in `[-1, 1]` (1 = identical).
- Per-caller isolation: `caller_plugin_id` from kernel, sanitized to `[a-zA-Z0-9_-]`.

## Config

| Env var | Default | Meaning |
|---|---|---|
| `VECTOR_DB_DATA_DIR` | — (required) | directory holding per-caller `.db` files |
| `VECTOR_DB_POOL_SIZE` | `4` | connections per caller pool |
| `VECTOR_DB_BUSY_TIMEOUT_MS` | `5000` | SQLite busy timeout |
| `VECTOR_DB_MAX_RESPONSE_BYTES` | `4194304` (4 MiB) | vec_query result size cap |
| `VECTOR_DB_MAX_DB_BYTES` | `268435456` (256 MiB) | hard per-caller disk quota via PRAGMA max_page_count; 0 disables |
| `VECTOR_DB_DEFAULT_DIM` | `384` | default dim for text-only upserts |

## Concurrency

Hot-path plugin: drives SDK `ConcurrentHandler` + `serve_concurrent` (vynkor-sdk ≥ 0.1.4).
One task owns `VynkorClient`, spawned handler tasks push responses via mpsc.

## Status

v0.1.0 — brute-force, fake embeddings, offline. See `ROADMAP.md` for v0.2 plan (real embeddings).
