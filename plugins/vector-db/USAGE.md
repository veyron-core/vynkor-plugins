# vector-db plugin — usage guide

Reference for plugin authors calling the `vector-db` plugin through the kernel.
For configuration, architecture and Ollama+ai integration, see [`README.md`](./README.md);
for what's deferred, see [`ROADMAP.md`](./ROADMAP.md).

## The model in one minute

- **One private SQLite file per caller.** Namespace is `caller_plugin_id` (kernel-stamped, `[a-zA-Z0-9_-]`), one `.db` file per caller under `VECTOR_DB_DATA_DIR`. Collections inside that file are namespaced by caller — `agent` never sees `notes` vectors.
- **One collection = one dimension.** First `vec_upsert` into a collection fixes its `dim` (e.g. 384 for fake, 768 for `nomic-embed-text`, 1536 for `text-embedding-3-small`). Mismatch on later upserts → `dimension mismatch`.
- **Storage is two tables:** `collections(name TEXT PRIMARY KEY, dim INT, created_at INT)` and `vectors(collection, id, text, vector, metadata, updated_at)` — vector stored as JSON array text, L2-normalized on write.
- **Search is brute-force cosine** (dot product on normalized vectors), sorted descending, `score` in `[-1,1]` (`1` = identical). Fine for 100k vectors; HNSW later.
- **Embeddings are pluggable:** `text` alone → `ai embedding` (Ollama/cloud) when `VECTOR_DB_EMBED_*` is set, otherwise `fake_embed` (hash-based, offline). `vector` alone → BYO (caller already embedded via `ai`). `vector` wins over `text` if both given.

## How a call looks

`ActionRequest { action, params_json }` → `ActionResponse`:

- success → `ACTION_OK`, `data_json` = JSON below
- failure → `ACTION_ERROR`, `error` = plain text (all messages listed in [Errors](#errors))

Examples show **params** and **result**.

## Actions

### `vec_upsert` — insert/update one document

```jsonc
// params — at least one of text or vector
{"collection":"mem","id":"note:42","text":"hello world","metadata":{"tag":"kernel"}}
{"collection":"mem","id":"note:42","vector":[0.012,-0.03,0.7,...],"text":"hello world"}
{"collection":"mem","id":"note:42","vector":[0.012,...],"metadata":{"source":"ollama"}}
// result
{"ok":true,"id":"note:42","dim":768}
```

- `collection` `1..128` `[a-zA-Z0-9_.-]`, `id` `1..256` (no NUL)
- `text` `1..10000` chars; empty/whitespace → ignored unless `vector` given
- `vector` `1..4096` floats, must be finite, non-zero norm; L2-normalized on write; must match collection `dim`
- `metadata` arbitrary JSON object, stored as text, filterable in queries
- Upsert: existing `(collection,id)` overwrites
- **Embedding path:** if `vector` given → validated only; if `text` only and `VECTOR_DB_EMBED_MODEL` set → `vector-db` → `ai embedding {provider,model,base_url,api_key_env,input:text}` → Ollama/cloud → normalized vector; else → `fake_embed(text, dim)` (deterministic, hash-based). `vector` always wins.
- First write creates collection with that `dim`; concurrent first-writes race-safe via `INSERT OR IGNORE`.

### `vec_query` — similarity search

```jsonc
// params — at least one of text or vector
{"collection":"mem","text":"hello","top_k":5}
{"collection":"mem","vector":[0.012,...],"top_k":5,"filter":{"tag":"kernel"},"include_vector":false}
// result
{"results":[{"id":"note:42","score":0.92,"text":"hello world","metadata":{"tag":"kernel"}}]}
// with include_vector:true → adds "vector":[0.012,...]
```

- `top_k` default `5`, `1..100`
- `filter` exact match on top-level metadata keys (`{"tag":"a"}` matches only `metadata.tag=="a"`)
- `include_vector` default `false` — when true, each result includes stored vector (costs extra SELECT)
- Embedding path same as `vec_upsert`: `vector` → validated, `text` → `ai` or `fake`; `dim` must match collection; missing collection → `{"results":[]}`
- Response capped by `VECTOR_DB_MAX_RESPONSE_BYTES` (4 MiB default) → `query result exceeds max_response_bytes`
- Scores are cosine similarity on normalized vectors.

### `vec_get` — fetch one document

```jsonc
// params
{"collection":"mem","id":"note:42"}
// result (found)
{"found":true,"id":"note:42","text":"hello world","vector":[0.012,...],"metadata":{...},"dim":768}
// result (missing)
{"found":false}
```

- `dim` derived from stored vector length.

### `vec_delete` — remove one document

```jsonc
// params
{"collection":"mem","id":"note:42"}
// result
{"deleted":true}  // false if missing — idempotent
```

- Publishes `changed` event (best-effort) only when a row was actually deleted (like `database`).

### `vec_list` — list collections

```jsonc
// params
{}                          // all collections
{"prefix":"agent_"}          // filtered
// result
{"collections":["agent_mem","mem"]}
```

### `vec_stats` — collection stats

```jsonc
// params
{"collection":"mem"}
// result (exists)
{"count":42,"dim":768}
// result (missing collection)
{"count":0,"dim":0}
```

## Embedding via Ollama + ai (рекомендуемый путь)

Модели ставят через Ollama, вызывают через `ai`, `vector-db` пересылает туда текст.

```bash
ollama pull nomic-embed-text   # 768 dim, 274M
ollama pull mxbai-embed-large  # 1024 dim, 669M, лучше качество
ollama pull all-minilm         # 384 dim, 46M, самый быстрый
ollama serve &
curl http://localhost:11434/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{"model":"nomic-embed-text","input":"hello"}'  # sanity check
```

```yaml
# config.yaml
plugins:
  - id: network
    binary: /opt/plugins/network
    sandbox: false
    env:
      - NETWORK_PLUGIN_ALLOWED_HOSTS=localhost,127.0.0.1  # иначе SSRF режет loopback
  - id: ai
    binary: /opt/plugins/ai
    sandbox: true
    env:
      - AI_PLUGIN_ALLOWED_KEY_ENVS=OLLAMA_API_KEY
      - OLLAMA_API_KEY=  # пустой для Ollama, но allowlist обязателен
  - id: vector-db
    binary: /opt/plugins/vector-db
    sandbox: true
    env:
      - VECTOR_DB_DATA_DIR=/var/lib/vyn/vector-db
      - VECTOR_DB_EMBED_MODEL=nomic-embed-text
      - VECTOR_DB_EMBED_PROVIDER=openai
      - VECTOR_DB_EMBED_BASE_URL=http://localhost:11434/v1
      - VECTOR_DB_EMBED_API_KEY_ENV=OLLAMA_API_KEY
      - VECTOR_DB_EMBED_TIMEOUT_MS=10000
      - VECTOR_DB_DEFAULT_DIM=768  # должен совпадать с dim модели
```

Проверка через `ai` напрямую:
```json
// ai embedding
{"provider":"openai","base_url":"http://localhost:11434/v1","model":"nomic-embed-text",
 "api_key_env":"OLLAMA_API_KEY","input":"hello world"}
// → {"embedding":[0.012,...],"dim":768,"model":"nomic-embed-text"}

// vector-db сам перешлёт в ai
{"action":"vec_upsert","collection":"mem","id":"1","text":"hello world"}
{"action":"vec_query","collection":"mem","text":"hello","top_k":5}
```

Двухшаговый BYO (работает всегда, даже без настройки `vector-db`):
```json
// 1) ai embedding (cloud или Ollama) → vector
// 2) vec_upsert {collection, id, vector, text}
```

Cloud без Ollama — тот же flow, но `base_url` на OpenAI/Voyage:
```json
{"provider":"openai","base_url":"https://api.openai.com/v1","model":"text-embedding-3-small",
 "api_key_env":"OPENAI_API_KEY","input":"hi"}
```

Офлайн fake и реальный `ai` вектора несовместимы: `dim` фиксируется первым upsert — смена модели → новая коллекция.

## Errors

All failures are `ACTION_ERROR` with these messages:

- `collection must be non-empty` / `collection too long` / `invalid collection name ...`
- `id must be non-empty` / `id too long` / `id must not contain null bytes`
- `vec_upsert requires at least one of text or vector` / `vec_query requires at least one of text or vector`
- `params.text too long (max 10000)` / `params.top_k must be 1..100` / `params.vector must be non-empty` / `vector dim too large: N > 4096`
- `vector must be non-empty` / `vector dim too large` / `vector[N] is not finite` / `vector has zero norm`
- `dimension mismatch: collection 'X' dim D, got N` / `dimension mismatch: expected D, got N`
- `query result exceeds max_response_bytes (> M)` / `vector too large`
- `unknown action: X` / `invalid params for vec_* ...` / `ai.embedding failed: ...` (wrapped, key never leaked)
- `missing caller_plugin_id` / `invalid caller_plugin_id`

Missing collection in `vec_query`/`vec_stats` is not an error — returns `{"results":[]}` / `{"count":0,"dim":0}`. `vec_get` missing → `{"found":false}`. `vec_delete` missing → `{"deleted":false}`.

## Config

See `config.example.yaml`. All env vars are read at startup from kernel `config.yaml` `env:`.

| Env var | Default | Meaning |
|---|---|---|
| `VECTOR_DB_DATA_DIR` | — (required) | per-caller `.db` files |
| `VECTOR_DB_POOL_SIZE` | `4` | sqlite pool size |
| `VECTOR_DB_BUSY_TIMEOUT_MS` | `5000` | busy timeout |
| `VECTOR_DB_MAX_RESPONSE_BYTES` | `4194304` | query result cap |
| `VECTOR_DB_MAX_DB_BYTES` | `268435456` | `PRAGMA max_page_count` quota, `0` disables |
| `VECTOR_DB_DEFAULT_DIM` | `384` | dim for fake embeddings |
| `VECTOR_DB_EMBED_MODEL` | — | if set, enables forwarding `text` → `ai embedding`; e.g. `nomic-embed-text` |
| `VECTOR_DB_EMBED_PROVIDER` | `openai` | `openai` only (covers Ollama/Voyage) |
| `VECTOR_DB_EMBED_BASE_URL` | — | e.g. `http://localhost:11434/v1` |
| `VECTOR_DB_EMBED_API_KEY_ENV` | — | must be in `AI_PLUGIN_ALLOWED_KEY_ENVS` |
| `VECTOR_DB_EMBED_TIMEOUT_MS` | `10000` | ai call timeout |

## Testing

`cargo test -p vector-db-plugin` — 25 lib + 1 bin (concurrent deadlock regression) = 26 tests, no network/model needed (fake embeddings). With `VECTOR_DB_EMBED_*` set, live test needs `ai` + `network` + Ollama running.

See `README.md` for architecture diagram and `ROADMAP.md` for v0.2 (HNSW, sqlite-vec, batch upsert).
