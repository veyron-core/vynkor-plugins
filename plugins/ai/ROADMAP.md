# ai plugin roadmap

Goal: give any Vynkor plugin a way to call an LLM (Claude/OpenAI/etc) via a
dedicated `ai` plugin, same pattern as `network` for HTTP — one blessed path,
provider quirks/auth/retries live in one place instead of every plugin
rolling its own client.

## Decision: reuse `network`, don't reinvent

`ai` plugin does **not** open its own sockets. It calls the kernel-routed
`http_request` action (owned by the `network` plugin) via
`VynkorClient::send_action` — the same helper `network`'s own callers would
use. Because `http_request` is gated by `PERMISSION_NETWORK`, and the
kernel's anti-laundering check (T-19) requires the *caller* to hold a gated
action's permission as well as the provider, `ai` declares
`"permissions": ["network"]` (Manifest v2: the per-action `permission` on
`network`'s `http_request` makes this data-driven — any caller without the
permission is denied). SSRF blocklist / redirect handling / retry-backoff /
response size caps in `network` apply for free.

(Historical note: the first draft of this section claimed the kernel checks
only the *provider's* permission and `ai` could declare none — that predates
T-19's requester-side check, landed in `vynkor` as
`fix: require requester permission for gated actions, not just provider`.)

Practical effect: `ai`'s `plugin.json` has `"permissions": ["network"]` —
the one permission it needs to invoke `network`'s gated action. Its other
declared capability is the `actions` it exposes to *its* callers (see
below).

## Naming

Plugin id: `ai`. Binary: `ai`. Mirrors `network`/`ping-pong-rs` — short,
matches the "one blessed path per capability" convention.

## v1 scope

- One generic action, e.g. `chat_completion`:

  Request (`ActionRequest.params_json`):
  ```json
  {
    "provider": "anthropic",
    "model": "claude-sonnet-5",
    "api_key_env": "ANTHROPIC_API_KEY",
    "messages": [{"role": "user", "content": "..."}],
    "max_tokens": 1024,
    "timeout_ms": 30000
  }
  ```
  - `api_key_env`: name of an env var the *ai plugin process* reads at
    call time. Caller never puts the raw key in the payload — same
    reasoning as not logging secrets. Key itself comes from the `env:`
    list on `ai`'s entry in `config.yaml` (same mechanism `network` uses
    for `NETWORK_PLUGIN_EXTRA_BLOCKED_HOSTS`).
  - `provider`: enum, `anthropic` first (this is the Claude API skill's
    home turf), `openai`-compatible second — different request/response
    shape per provider, translated internally to the `network` plugin's
    `http_request` JSON.

  Response (`ActionResponse.data_json`) on success:
  ```json
  { "content": "...", "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 2} }
  ```

- Internally: build the provider-specific HTTP request (url, headers incl
  `x-api-key`/`Authorization`, JSON body), call
  `client.send_action("http_request", &params, timeout_ms)` against the
  kernel, parse `network`'s response, map to the shape above.
- Errors: bad/missing `api_key_env` var, provider HTTP error, malformed
  provider JSON → `ACTION_ERROR` with a human-readable message. Don't leak
  the key value in any error string.

## v1 implementation design (confirmed 2026-07-07)

**Crate layout** (mirrors `network`):

```
plugins/ai/
  Cargo.toml          # bin `ai`, lib `ai_plugin`
  plugin.json          # permissions: [], actions: ["chat_completion"]
  src/
    main.rs             # AiPlugin + custom serve loop
    request.rs           # parse_request: validate ChatCompletionParams
    provider/
      mod.rs               # Provider trait
      anthropic.rs          # Anthropic Messages API adapter
      openai_compat.rs      # OpenAI-compatible adapter (openai/openrouter/ollama/self-hosted)
    handler.rs            # parse_request -> provider -> network's http_request -> parse_response
```

**Custom serve loop, not `Plugin::run`.** The SDK's `Plugin::on_message` only
gets `&mut self`, not `&mut VynkorClient` — confirmed against the kernel
(`vynkor/src/plugins/registry.rs:86-98`, `vynkor/src/ipc/protocol.rs:239`)
that a plugin cannot open a second connection under the same `plugin_id`
(registration rejected) nor send `ActionRequest`s over an unregistered
connection (routing rejected). So there is no way to get a second
`VynkorClient` for the outbound `send_action` call into `network`. `ai`'s
`main.rs` implements its own loop — near-identical to the SDK's `serve()`
(ping/pong, event ack, shutdown) — but calls the handler with `&mut client`
in hand. Sequential, one request at a time, same model as `network` and
`ping-pong-rs` already use.

**Provider scope:** two adapters, not four. `anthropic` (its own wire shape)
and one generic `openai`-compatible adapter with a caller-supplied
`base_url` — covers OpenAI, OpenRouter, and Ollama (all speak
`/v1/chat/completions`), plus any other self-hosted OpenAI-compatible
gateway, without a dedicated module per vendor.

**Request** (`ActionRequest.params_json` for `chat_completion`):
```json
{
  "provider": "anthropic" | "openai",
  "base_url": "https://api.openai.com/v1",
  "model": "claude-sonnet-5",
  "api_key_env": "ANTHROPIC_API_KEY",
  "messages": [{"role": "user", "content": "..."}],
  "max_tokens": 1024,
  "timeout_ms": 30000
}
```
- `anthropic`: fixed default `base_url` (`https://api.anthropic.com`),
  overridable. `openai`: `base_url` required (no safe default across
  OpenAI/OpenRouter/Ollama).
- `api_key_env` only — no literal `api_key` param. Caller never puts the
  raw key in the payload; the `ai` process reads its own env at call time.
  Missing/unset env var → `ACTION_ERROR`, key value never appears in any
  error string.

**Provider trait**:
```rust
pub trait Provider {
    fn build_http_request(&self, params: &ChatParams, api_key: &str) -> HttpRequestJson;
    fn parse_response(&self, body: &[u8]) -> Result<ChatResult, String>;
}
```
- `anthropic`: `POST {base_url}/v1/messages`, `x-api-key` + `anthropic-version`
  headers; response `content[0].text`, `stop_reason`,
  `usage.{input,output}_tokens`.
- `openai_compat`: `POST {base_url}/chat/completions`, `Authorization: Bearer`
  header (omitted when `api_key_env` resolves empty, e.g. local Ollama with
  no auth); response `choices[0].message.content`,
  `choices[0].finish_reason`, `usage.{prompt,completion}_tokens`.

**Response** (`ActionResponse.data_json`) — both adapters normalize to:
```json
{ "content": "...", "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 2} }
```

**Errors → `ACTION_ERROR`:** missing/malformed request, unset `api_key_env`,
malformed provider JSON, non-2xx HTTP status from the provider, and any
error `network`'s `http_request` itself returns (SSRF block, timeout, DNS
failure) — bubbled up with `network`'s message, never swallowed as a
200-with-error-body.

**Testing:** unit tests per adapter (`build_http_request`/`parse_response`
against fixture JSON, no live network — the actual send is `network`'s job),
`request.rs` validation tests mirroring `network/src/request.rs`'s style.

## Near-term (buildable now, no kernel changes)

- **Vision input** — **shipped in 0.1.1**: `messages[].content` accepts
  image content blocks (`{"type":"image","mime_type":...,"data_base64":...}`,
  png/jpeg/gif/webp, ≤8 per message, ≤5 MiB decoded each), adapted to
  anthropic `image` source blocks / openai `image_url` data URLs.
- **Native tool-use passthrough** — **shipped in 0.1.1**: `tools` in
  (`{name, description?, input_schema?}`), model invocations out as
  normalized `tool_calls` (`{id, name, arguments_json}`); omitted when empty
  so plain-text responses keep the pre-tools shape.
- **Retry-on-429** — **shipped in 0.1.1**: `max_retries` (default 2) /
  `retry_backoff_ms` (default 1000) forwarded to `network`'s `http_request`,
  which re-sends on 429/transient-5xx with doubling backoff; LLM-appropriate
  longer default backoff than network's own 200 ms.
- **Image generation (`image_gen`)** — second action hitting provider image
  endpoints. OpenAI-compatible only (`POST {base_url}/images/generations` —
  covers OpenAI and local Stable Diffusion gateways; anthropic has no gen
  API). Returns base64 or URL per provider; same key resolution and gated
  `http_request` routing as `chat_completion`.
- **Streaming responses** — provider APIs support SSE token streaming;
   today's model is one `ActionRequest` → one `ActionResponse`, so v1 has to
   buffer the full completion before replying. Fine for v1, revisit once
   R6-02 (`ActionStreamChunk`, see `vynkor/ROADMAP.md`) lands in the kernel.
- **Token/cost accounting** — log `usage` fields per call; no event bus
  path yet (see below), so stdout JSON logging like `network` does today.

## Requires kernel/protocol changes

- **Streaming action support (R6-02)** — real token-by-token streaming to
  the caller instead of buffer-then-reply. Blocked on the same
  `ActionStreamChunk` (or raw-channel) primitive `network`'s
  `http_request_stream` follow-up needs — see `vynkor/ROADMAP.md` R6-02.
- **`ai.completion_done` events (R6-01)** — publish usage/latency/model to
  the event bus for observability, same blocker as `network`'s
  `network.request_completed` (plugin → event-bus publish path, R6-01,
  design doc already exists: `vynkor/docs/superpowers/specs/2026-07-06-plugin-event-publish-design.md`).
- **Per-caller quotas (R6-03)** — if `ai` becomes a shared resource for many
  plugins, same open question as `network`'s per-caller concurrency cap:
  `ActionRequest` has no caller-id field to key on yet.

## Non-goals / follow-ups

- No kernel special-casing for "AI" — manifesto is zero-AI in the kernel
  core, and `PERMISSION_AI`/`needs_ai` were already tried and retired
  (`reserved` in `vynkor_protocol.proto`). `ai` is an ordinary plugin like
  any other.
- Tool-use / function-calling passthrough — v2, once basic chat completion
  is solid.
- Multi-turn conversation state / memory — caller's responsibility (the
  plugin calling `ai`), not this plugin's job to hold session state.
