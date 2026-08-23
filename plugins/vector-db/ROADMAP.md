# vector-db roadmap

## v0.1 — shipped (0.1.0, версия не меняется)

- Per-caller SQLite collections + brute-force cosine, 6 actions: vec_upsert/query/get/delete/list/stats
- Offline fake embeddings (hash-based, L2-norm) — 0 deps, работает без сети/ai
- **Через ai (Ollama) — модели ставят через `ollama pull`, вызывают через `ai embedding`, `vector-db` пересылает туда `text`:** `POST {base_url}/embeddings` via `ai` (vault-first, `AI_PLUGIN_ALLOWED_KEY_ENVS`), `network` SSRF-guard; `nomic-embed-text` 768 / `mxbai-embed-large` 1024 / `all-minilm` 384, cloud `text-embedding-3-small` 1536 — единый `openai` адаптер. В `vector-db` — `VECTOR_DB_EMBED_*` (provider/model/base_url/api_key_env/timeout) + fallback в fake; `vector` в запросе всегда BYO и не трогает `ai`
- Архитектура: `vector-db` — single-reader loop + `Rpc` proxy (как `notes`/`calendar`), `Handler::handle_with_rpc` → `embed_via_ai_or_fake`; `ConcurrentHandler` оставлен для тестов/hot-path без `ai`
- Per-caller isolation, WAL, `max_page_count` quota, `INSERT OR IGNORE` race-safe для collections
- 26 tests incl. concurrent deadlock regression, per-caller isolation, dim mismatch, filter/top_k
- Docs: `README` (архитектура Ollama→ai→vector-db, 3-командный setup), `USAGE` (все actions + Ollama + BYO), `config.example.yaml` (VECTOR_DB_EMBED_*)

## v0.2 — локальный ONNX + производительность (без смены протокола)

- `fastembed` локальный ONNX без `ai`: `bge-small-en-v1.5` 384 default, `all-MiniLM-L6-v2`, `multilingual-e5-small`; кастом via `VECTOR_DB_MODEL_DIR` (model.onnx + tokenizer.json)
- BYO уже в v0.1, cloud уже через `ai` — в v0.2 добавить прямой `VECTOR_DB_*` без `ai` как альтернативу
- Trait `Embedder` dispatch (local ONNX vs ai vs BYO) вместо `fake_embed` hardcode

## v0.3 — performance

- `sqlite-vec` vec0 virtual table or `usearch` HNSW for 100k+ vectors
- Hybrid search: vector + metadata filter pushdown
- Batch upsert `vec_upsert_batch`

Non-goals: no server-side reranking (caller does it), no auto-embedding of `database`/`notes` (caller pushes explicitly).
