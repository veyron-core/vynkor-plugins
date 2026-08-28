# ai plugin — usage guide

Reference for plugin authors calling the `ai` plugin through the kernel. For
the design rationale (why `ai` reuses `network` instead of opening its own
sockets), operator configuration, and the security model behind
`api_key_env`, see [`README.md`](./README.md); for what's deferred
(streaming, tool-use, retries), see [`ROADMAP.md`](./ROADMAP.md).

## The model in one minute

- **Two actions, `chat_completion` + `embedding`.** `chat_completion`: messages → completion (no state). `embedding`: single text → vector (for `vector-db`). Both provider-agnostic and vault-first.
- **Two providers for chat, one for embeddings.** Chat: `anthropic` (Claude Messages API) and `openai` (OpenAI-compatible — covers OpenAI, OpenRouter, Ollama). Embeddings: `openai` only — covers OpenAI `text-embedding-3-*`, Voyage, and local Ollama (`nomic-embed-text` 768, `mxbai-embed-large` 1024, `all-minilm` 384) via the same `/v1/embeddings` shape. `anthropic` → `anthropic does not support embeddings`.
- **Vector-db integration.** `vector-db` `vec_upsert {text}` / `vec_query {text}` forwards to `ai embedding` when `VECTOR_DB_EMBED_*` is set (Ollama at `http://localhost:11434/v1`). See `plugins/vector-db/README.md` “Архитектура эмбеддинга: Ollama → ai → vector-db” and `plugins/vector-db/USAGE.md`.
- **You never send the API key.** You send the *name* of an env var
  (`api_key_env`); the `ai` process reads it at call time. The operator must
  have allowlisted that name (`AI_PLUGIN_ALLOWED_KEY_ENVS`) and set the value.
  The raw key never appears in your payload, in logs, or in any error string.
- **`ai` needs `network`.** Every call is routed through the `network`
  plugin's `http_request` action. `network` must be registered and running,
  and its SSRF blocklist / host allowlist applies to the `base_url` you pick.

## How a call looks

You send an `ActionRequest { action: "chat_completion", params_json }`. You
get back an `ActionResponse`:

- success → status `ACTION_OK`, `data_json` = the normalized result below.
- failure → status `ACTION_ERROR`, `error` = a plain-text message (every
  message a caller can hit is listed in [Errors](#errors)).

Examples below show **params** (what goes in `params_json`) and **result**
(the JSON returned in `data_json`).

## `chat_completion` — request

```jsonc
// params
{
  "provider": "anthropic",              // required: "anthropic" | "openai"
  "base_url": "https://api.anthropic.com", // optional for anthropic, required for openai
  "model": "claude-sonnet-5",           // required, non-empty
  "api_key_env": "ANTHROPIC_API_KEY",   // required: env var NAME, not the key
  "messages": [                          // required, non-empty
    {"role": "user", "content": "Explain SQLite WAL mode in one sentence."}
  ],
  "max_tokens": 1024,                    // optional, default 1024, capped at 8192
  "timeout_ms": 30000                    // optional, default and cap 30000
}
```

| Field | Required | Notes |
|---|---|---|
| `provider` | yes | `"anthropic"` or `"openai"`. Anything else → error. |
| `base_url` | openai only | `anthropic` defaults to `https://api.anthropic.com`; `openai` has no safe default (OpenAI vs OpenRouter vs Ollama), so it's required. A trailing `/` is trimmed. |
| `model` | yes | Provider's model id. Not validated by `ai` — a bad model surfaces as the provider's own HTTP error. |
| `api_key_env` | yes | Name of an env var the `ai` process reads. Must be on the operator's `AI_PLUGIN_ALLOWED_KEY_ENVS` allowlist. See [README's Configuration](./README.md#configuration). |
| `messages` | yes | Non-empty array of `{role, content}`. `content` is a string **or** an array of blocks — text (`{"type":"text","text":...}`) and images (`{"type":"image","mime_type":"image/png|jpeg|gif|webp","data_base64":"..."}`, max 8/message, 5 MiB decoded each). |
| `tools` | no | Native tool definitions: `{name, description?, input_schema?}`. Max 64; schemas ≤ 32 KiB, names must be unique. The model's invocations come back as output `tool_calls`. |
| `max_tokens` | no | Default `1024`. Values above `8192` are **clamped**, not rejected. |
| `timeout_ms` | no | Default and hard cap `30000`. Higher values are clamped to `30000` — the wrapping `network` call can't outlive that. |
| `max_retries` / `retry_backoff_ms` | no | Retry policy for HTTP 429/transient 5xx, executed by `network` with doubling backoff. Default `2` retries from `1000` ms; caps `5` / `5000`. |

## Vision example

```jsonc
// params
{
  "provider": "openai",
  "base_url": "https://api.openai.com/v1",
  "model": "gpt-4o",
  "api_key_env": "OPENAI_API_KEY",
  "messages": [{
    "role": "user",
    "content": [
      {"type": "text", "text": "What's in this screenshot?"},
      {"type": "image", "mime_type": "image/png", "data_base64": "<base64 PNG>"}
    ]
  }]
}
```

## Tool-use example

```jsonc
// params — declare tools…
"tools": [{"name": "launch", "description": "Launch an app by id",
           "input_schema": {"type":"object","properties":{"app_id":{"type":"string"}},"required":["app_id"]}}]
// …and the reply carries the invocation instead of (or alongside) text:
{
  "content": "",
  "tool_calls": [{"id": "call_1", "name": "launch", "arguments_json": "{\"app_id\":\"firefox\"}"}],
  "stop_reason": "tool_calls"
}
```

## `chat_completion` — response

Normalized across both providers:

```jsonc
// result
{
  "content": "SQLite WAL mode writes changes to a separate log first...",
  "stop_reason": "end_turn",
  "usage": {"input_tokens": 17, "output_tokens": 42}
}
```

`tool_calls` appears only when the model requested invocations; plain-text
responses keep exactly this shape.

## `embedding` — request (for vector-db, Ollama)

```jsonc
// params — via ai, model lives in Ollama
{
  "provider": "openai",                         // only "openai" for embeddings
  "base_url": "http://localhost:11434/v1",      // Ollama OpenAI-compat, or https://api.openai.com/v1
  "model": "nomic-embed-text",                  // 768 dim — or mxbai-embed-large 1024, all-minilm 384, text-embedding-3-small 1536
  "api_key_env": "OLLAMA_API_KEY",              // must be in AI_PLUGIN_ALLOWED_KEY_ENVS; Ollama: "" (empty, still allowlisted)
  "input": "hello world",                       // required, 1..10000 chars, single text (batch later)
  "timeout_ms": 10000                           // optional, default/cap 30000
}
// also supports agent_id / stored model_id (like chat_completion):
{"agent_id":"embed","input":"hello world"}
{"model":"nomic-embed-text","input":"hello world"}  // resolves base_url/api_key_env from ai.db if model there
```

| Field | Required | Notes |
|---|---|---|
| `provider` | yes (unless `agent_id`) | `"openai"` only for embeddings. `anthropic` → error. |
| `base_url` | openai | No default; `http://localhost:11434/v1` for Ollama. Trailing `/` trimmed. |
| `model` | yes (unless `agent_id`) | Embedding model id. With Ollama: `nomic-embed-text` (default), `mxbai-embed-large`, `all-minilm`. With OpenAI: `text-embedding-3-small` (1536), `text-embedding-3-large` (3072). |
| `api_key_env` | yes (unless `agent_id`) | Env var name, vault-first, must be in `AI_PLUGIN_ALLOWED_KEY_ENVS`. Ollama: pick `OLLAMA_API_KEY=""` (empty) — still add to allowlist. |
| `input` | yes | Single text `1..10000` chars. Empty → `input must not be empty`. |
| `timeout_ms` | no | Default `30000`, cap `30000`. |

## `embedding` — response

Normalized (OpenAI shape):

```jsonc
// result
{
  "embedding": [0.012, -0.03, 0.007, ...],  // length = dim, L2-normalizable, finite
  "dim": 768,
  "model": "nomic-embed-text",
  "usage": {"input_tokens":2,"output_tokens":0}
}
```

- `embedding` is the raw provider vector (not yet normalized — `vector-db` normalizes on write).
- `dim` is `embedding.length` (768/1024/384/1536 depending on model).
- `vector-db` forwards `text` → `ai embedding` when `VECTOR_DB_EMBED_MODEL` is set (see `vector-db/README.md`). Direct `vector` in `vec_upsert` always bypasses `ai`.

Ollama setup (once):
```bash
ollama pull nomic-embed-text  # 274M, 768
ollama pull mxbai-embed-large # 669M, 1024
ollama serve &
# network must allow loopback: NETWORK_PLUGIN_ALLOWED_HOSTS=localhost,127.0.0.1
```

- `content` — the completion text. For `anthropic` it's the first content
  block's `text`; for `openai` it's `choices[0].message.content`.
- `stop_reason` — provider's stop reason, passed through as-is
  (`anthropic`: `end_turn`/`max_tokens`/…; `openai`: `stop`/`length`/…).
  Empty string if the provider omitted it.
- `usage.input_tokens` / `usage.output_tokens` — token counts. Mapped from
  `anthropic`'s `usage.{input,output}_tokens` and `openai`'s
  `usage.{prompt,completion}_tokens`. `0` if the provider didn't report them.

## Providers

### `anthropic` — Claude Messages API

- HTTP: `POST {base_url}/v1/messages`, `base_url` defaults to
  `https://api.anthropic.com`.
- Auth header: `x-api-key: <resolved key>`, plus a pinned
  `anthropic-version` header.

```jsonc
// params
{
  "provider": "anthropic",
  "model": "claude-sonnet-5",
  "api_key_env": "ANTHROPIC_API_KEY",
  "messages": [{"role": "user", "content": "hi"}]
}
```

### `openai` — OpenAI-compatible chat completions

One adapter covers OpenAI, OpenRouter, self-hosted gateways, and local
Ollama — all speak `/chat/completions`.

- HTTP: `POST {base_url}/chat/completions`. `base_url` is **required**.
- Auth header: `Authorization: Bearer <resolved key>` — **omitted entirely**
  when the resolved key is empty, so a local Ollama with no auth works
  without sending a bogus `Bearer ` token.

```jsonc
// OpenAI
{
  "provider": "openai",
  "base_url": "https://api.openai.com/v1",
  "model": "gpt-4o",
  "api_key_env": "OPENAI_API_KEY",
  "messages": [{"role": "user", "content": "hi"}]
}
```

```jsonc
// Local Ollama — see README's "Talking to a local model" for the network
// config (loopback is SSRF-blocked by default) and the empty-key setup.
{
  "provider": "openai",
  "base_url": "http://localhost:11434/v1",
  "model": "deepseek-coder:1.3b",
  "api_key_env": "OLLAMA_API_KEY",
  "messages": [{"role": "user", "content": "hi"}]
}
```

## Brain options by price/speed

All four options below speak the `openai` wire shape — same adapter, only
`base_url` / `model` / `api_key_env` change. Public hosts are reachable
through `network`'s default SSRF policy (only private ranges are blocked);
the local Ollama option additionally needs the loopback allow from
[Providers](#providers).

| Option | Price | Speed | Tool-calls | Notes |
|---|---|---|---|---|
| Ollama (local) | 0 | CPU-bound | yes (model-dependent) | fully offline, no key |
| Gemini free tier | 0 | fast | yes (compat layer) | rate-limited per minute |
| Groq free tier | 0 | very fast (LPU) | yes | smaller open models |
| OpenRouter | pay-per-token | varies by model | yes | one key → hundreds of models |

Every recipe needs its handle in `AI_PLUGIN_ALLOWED_KEY_ENVS` and the key
stored vault-first or in `env:` (see [Configuration](#configuration) in the
README). Model names move fast — treat them as examples and check the
provider's current list.

```jsonc
// Local Ollama — 0, offline. network env needs:
//   NETWORK_PLUGIN_ALLOWED_HOSTS=localhost,127.0.0.1
{
  "provider": "openai",
  "base_url": "http://localhost:11434/v1",
  "model": "qwen3:8b",
  "api_key_env": "OLLAMA_API_KEY",
  "messages": [{"role": "user", "content": "hi"}]
}
```

```jsonc
// Gemini free tier — 0. OpenAI-compatible endpoint; key from
// Verified live 2026-08-26: chat answered in ~6s, embeddings via
// gemini-embedding-001 (3072-dim) work through ai.embedding.
// Free tier is rate-limited (~10-15
// req/min on flash models), so keep max_retries >= 2 and expect 429s on
// bursty agent loops.
//
// Caveats of the compat layer: thinking models spend the max_tokens budget
// on internal thoughts too — set it generously (4096+) or replies come back
// empty-truncated; tool-calls work but the function schema subset is narrower
// than native — verify per model before relying on complex input_schema.
{
  "provider": "openai",
  "base_url": "https://generativelanguage.googleapis.com/v1beta/openai",
  "model": "gemini-3.5-flash-lite",
  "api_key_env": "GEMINI_API_KEY",
  "max_tokens": 4096,
  "messages": [{"role": "user", "content": "hi"}]
}
```

```jsonc
// Groq free tier — 0, fastest tokens/sec of the free options. Key from
// https://console.groq.com/keys .
{
  "provider": "openai",
  "base_url": "https://api.groq.com/openai/v1",
  "model": "llama-3.3-70b-versatile",
  "api_key_env": "GROQ_API_KEY",
  "messages": [{"role": "user", "content": "hi"}]
}
```

```jsonc
// OpenRouter — pay-per-token aggregator; one key covers Claude, Gemini,
// GPT, open models. Model ids are "vendor/model". Key from
// https://openrouter.ai/keys .
{
  "provider": "openai",
  "base_url": "https://openrouter.ai/api/v1",
  "model": "anthropic/claude-sonnet-4.5",
  "api_key_env": "OPENROUTER_API_KEY",
  "messages": [{"role": "user", "content": "hi"}]
}
```

Running the `agent` plugin on top of any of these works unchanged — point
`agent`'s model config at the same `base_url`/`model` pair (see
`plugins/agent/config.example.yaml`). For a first end-to-end check after
switching providers, run one small goal through the kernel rather than a
bare `chat_completion`: that exercises vision/tools/retry paths too.

## Errors

Every failure is an `ACTION_ERROR` with a plain-text `error`. Messages are
stable enough to branch on by substring rather than exact-matching. The
resolved API key value never appears in any of them.

**Bad request (fix the params):**

| Message (shape) | Cause |
|---|---|
| `invalid JSON: …` | `params_json` isn't valid JSON |
| `missing required field: provider` | `provider` absent (same shape for `model`, `api_key_env`, `messages`) |
| `unsupported provider: <name>` | `provider` isn't `anthropic` or `openai` |
| `missing required field: base_url` | `openai` request without a `base_url` |
| `model must not be empty` | `model` present but empty string |
| `api_key_env must not be empty` | `api_key_env` present but empty string |
| `messages must not be empty` | `messages` is `[]` |
| `input must not be empty` | `embedding` `input` is `""` / whitespace |
| `input too long (max 10000)` | `embedding` `input` > 10000 chars |
| `anthropic does not support embeddings` | `embedding` with `provider: anthropic` |
| `unknown action: <name>` | action wasn't `chat_completion` or `embedding` |

**Rejected by policy / key resolution:**

| Message (shape) | Cause | Recover by |
|---|---|---|
| `api_key_env '<name>' is not in the operator's AI_PLUGIN_ALLOWED_KEY_ENVS allowlist` | the env var name you passed isn't allowlisted by the operator | ask the operator to add it to `AI_PLUGIN_ALLOWED_KEY_ENVS`, or use an allowlisted name |
| `environment variable <name> is not set` | name is allowlisted but has no value in the `ai` process env | ask the operator to set it (Ollama-style empty keys are fine — this only fires for a genuinely unset var) |

**Upstream (network / provider), surfaced as-is:**

| Message (contains) | Cause | Recover by |
|---|---|---|
| `network plugin call failed: …` | the `http_request` action couldn't be reached (network plugin down, kernel routing error) | make sure `network` is registered and running |
| `network plugin error: …` | `network` returned `ACTION_ERROR` — SSRF block, DNS failure, connection refused, its own timeout | check `base_url` against `network`'s host allowlist/blocklist (see `plugins/network/README.md`) |
| `provider returned HTTP <code>: <body>` | provider answered non-2xx (bad key → 401, bad model → 404, rate limit → 429) | fix per the provider's body; there's no automatic 429 retry yet (see ROADMAP) |
| `malformed anthropic response: …` / `malformed openai-compatible response: …` | provider's 2xx body didn't match the expected shape | usually a wrong `base_url` pointed at a non-LLM endpoint |
| `anthropic response has no content blocks` / `openai-compatible response has no choices` | provider returned an empty completion | inspect the request; often a content-filter or truncation on the provider side |
| `malformed network response: …` / `malformed base64 response body: …` | `network`'s own response envelope didn't decode | report it — indicates a `network`/`ai` version mismatch |

## Recipes

### Multi-turn conversation

`ai` holds no session state — keep the running transcript yourself and resend
it each call, appending the model's `content` as an `assistant` turn:

```jsonc
{
  "provider": "anthropic",
  "model": "claude-sonnet-5",
  "api_key_env": "ANTHROPIC_API_KEY",
  "messages": [
    {"role": "user", "content": "What's the capital of France?"},
    {"role": "assistant", "content": "Paris."},
    {"role": "user", "content": "And its population?"}
  ]
}
```

### Provider-agnostic calls

Because both adapters return the same `{content, stop_reason, usage}` shape,
you can pick the provider at runtime (config, cost, availability) and leave
your response handling untouched — only `provider`/`base_url`/`model`/
`api_key_env` change.

### System prompts

Pass a system/developer turn as the first message (`{"role": "system",
...}`) — it's forwarded to the provider verbatim. `ai` doesn't hoist it into
a separate field, so use whatever role the target provider expects.

## FAQ

**Do I put my API key in the request?** No. You pass `api_key_env` — the
*name* of an env var. The `ai` process reads the value at call time. The key
never travels in your payload and never lands in a log or error string.

**Why is my key rejected even though it's set?** The name must also be on the
operator's `AI_PLUGIN_ALLOWED_KEY_ENVS` allowlist (default-deny). Allowlisted
but unset → `environment variable … is not set`; set but not allowlisted →
`… is not in the operator's … allowlist`.

**Can I stream tokens?** Not in v1 — one `ActionRequest` → one buffered
`ActionResponse`. Streaming is blocked on a kernel primitive (see ROADMAP,
"Requires kernel/protocol changes").

**Does it retry on 429?** No. A provider `429` comes back as
`provider returned HTTP 429: …`; back off and retry yourself. Automatic
backoff is a ROADMAP item.

**Why did a local Ollama call fail with an SSRF/blocked error?** `network`
blocks loopback by default. Its `env:` needs
`NETWORK_PLUGIN_ALLOWED_HOSTS=localhost,127.0.0.1` — see the README's
"Talking to a local model" section.

**Is the response ordered / can I fire several at once?** `ai`'s loop is
sequential — it handles one `chat_completion` at a time. Concurrent calls
from your side are serviced one after another.

**What about tool-use / function calling?** Not in v1 — it's a planned v2
passthrough (see ROADMAP non-goals).

## v0.3 — models, agents, analytics

Since v0.3 the plugin keeps a SQLite store (`<kernel data_dir>/plugins/ai/ai.db`)
with models, agent profiles and per-call token usage. The kernel grants the
plugin a writable data dir automatically (`VYN_DATA_DIR`).

**Model resolution.** `chat_completion` can name a **model id** or an
**agent id** instead of the legacy provider/base_url/api_key_env triple. The
plugin resolves the endpoint and key from its database, so a caller never
sends provider details:

```json
{ "model": "llama3.2", "messages": [{"role": "user", "content": "hi"}] }
{ "agent_id": "code", "messages": [{"role": "user", "content": "hi"}] }
```

Agent profiles carry a system prompt that the plugin injects (OpenAI:
`system` message; Anthropic: top-level `system` field).

**Other actions:**

- `list_models` → `[{id, provider, base_url, api_key_env, is_default, discovered_at, last_seen}]`
- `list_agents` → `[{id, name, model_id, system_prompt, goal, description, is_default}]`
- `refresh_models` → pulls configured providers' model lists and upserts
  them: `{discovered, updated, errors}`
- `usage_stats` → `{totals, by_model, by_agent}` — each bucket has
  `{requests, input_tokens, output_tokens}`

**Configuration** (env, set in `plugins.d/ai.yaml`): `AI_PLUGIN_MODELS`
(hand-declared models, required for Anthropic — no discovery API),
`AI_PLUGIN_AGENTS` (agent profiles), `AI_PLUGIN_DISCOVERY`
(`[{"provider":"ollama","base_url":"http://localhost:11434"}]` — ollama:
`GET /api/tags`; openai: `GET /models`). See `config.example.yaml`.
