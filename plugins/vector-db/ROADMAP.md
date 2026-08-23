# vector-db roadmap

## v0.1 — shipped

- Per-caller SQLite collections + brute-force cosine
- Actions: vec_upsert/query/get/delete/list/stats
- Fake hash-based embeddings (deterministic, L2-normalized) — offline, no native deps
- ConcurrentHandler, per-caller isolation, WAL, max_page_count quota
- 42 tests incl. concurrent deadlock regression

## v0.2 — real embeddings

- `fastembed` local ONNX: `bge-small-en-v1.5` (384) default, `all-MiniLM-L6-v2` alt, `multilingual-e5-small`
- Custom model via `VECTOR_DB_MODEL_DIR` (model.onnx + tokenizer.json)
- BYO vector mode already in v0.1 (pass `vector` directly)
- Optional cloud embeddings: `network` http_request + `secrets` vault-first (OPENAI_API_KEY etc.), `VECTOR_DB_ALLOWED_KEY_ENVS` allowlist, same pattern as `ai`/`search`
- Replace fake_embed with trait `Embedder` dispatch (local vs cloud vs BYO)

## v0.3 — performance

- `sqlite-vec` vec0 virtual table or `usearch` HNSW for 100k+ vectors
- Hybrid search: vector + metadata filter pushdown
- Batch upsert `vec_upsert_batch`

Non-goals: no server-side reranking (caller does it), no auto-embedding of `database`/`notes` (caller pushes explicitly).
