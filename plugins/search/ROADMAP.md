# search plugin roadmap

Goal: give any vynkor plugin a way to run a web search — one blessed path,
provider quirks/auth in one place instead of every plugin rolling its own
client. Same architecture as `ai`/`tts`/`stt`: no sockets of its own, all
HTTP routed through `network`'s gated `http_request`.

## Decision: reuse `network`, don't reinvent

`search` does **not** open its own sockets. It calls the kernel-routed
`http_request` action (owned by the `network` plugin) via
`VynkorClient::send_action` — the same helper `ai`/`tts`/`stt` use. Because
`http_request` is gated by `PERMISSION_NETWORK`, and the kernel's
anti-laundering check (T-19) requires the *caller* to hold a gated action's
permission as well as the provider, `search` declares
`"permissions": ["network", "secrets"]` (Manifest v2: the per-action
`permission` on `network`'s `http_request` and `secrets`' `secret_get`
makes this data-driven — any caller without the permission is denied).
SSRF blocklist / redirect handling / retry-backoff / response size caps in
`network` apply for free.

`secrets` is held for the same reason: `search` resolves provider keys
vault-first via `secret_get` (gated by `PERMISSION_SECRETS`), with the
process environment as fallback — identical to `ai`/`tts`/`stt`.

## Naming

Plugin id: `search`. Binary: `search`. Mirrors `network`/`ai`/`tts`/`stt` —
short, matches the "one blessed path per capability" convention. Env-var
prefix `SEARCH_PLUGIN_*` keeps the established spelling (the vynkor rename
doesn't touch protocol/config surfaces).

## v1 scope

- One action, `web_search`:

  Request (`ActionRequest.params_json`):
  ```json
  {
    "query": "vynkor plugin kernel",
    "provider": "brave",
    "api_key_env": "SEARCH_BRAVE_KEY",
    "count": 5,
    "timeout_ms": 30000
  }
  ```
  - `api_key_env`: name the `search` process resolves at call time
    (vault-first, env fallback), allowlisted via
    `SEARCH_PLUGIN_ALLOWED_KEY_ENVS`. Caller never puts the raw key in the
    payload — same reasoning as `ai`/`tts`.
  - `provider`: `brave` first, `tavily` second — different request shape per
    provider, translated internally to `network`'s `http_request` JSON.

  Response (`ActionResponse.data_json`) on success, normalized:
  ```json
  { "query": "vynkor plugin kernel", "results": [{ "title": "...", "url": "...", "snippet": "..." }] }
  ```

- **Provider adapters** (one module each, fixture-tested):
  - `brave`: `GET {base_url}/res/v1/web/search?q=<query>&count=<count>`,
    header `X-Subscription-Token`. Parses `web.results[]` →
    `title`/`url`/`description`.
  - `tavily`: `POST {base_url}/search`, JSON body
    `{"query", "max_results"}`, header `Authorization: Bearer`. Parses
    `results[]` → `title`/`url`/`content`.
  - Wire shapes verified against the providers' current docs before coding
    (sources cited in each adapter's module doc).

- **Key handling** (`src/key_resolve.rs`, adapted from `ai`): vault-first
  `secret_get` against the `secrets` plugin, env-var fallback of the same
  name; vault wins; resolved per request (no cache). The resolved value
  never appears in an error string or log line.

- **Errors → `ACTION_ERROR`:** missing/malformed request, un-allowlisted or
  unset `api_key_env`, malformed provider JSON, non-2xx HTTP status from
  the provider, and any error `network`'s `http_request` returns — bubbled
  up with `network`'s message, never swallowed.

- **Testing:** unit tests per adapter (`build_http_request` /
  `parse_response` against fixture JSON, no live network), `request.rs`
  validation + allowlist tests, and a fake-kernel `UnixStream::pair`
  integration test driving the full handler end to end.

## Near-term (buildable now, no kernel changes)

- **More providers** — `serpapi`, `serper`, `duckduckgo`: one `Provider`
  impl each, one file, same trait. Deferred until a caller needs them.
- **Retry-on-429** — mirror `network`'s retry/backoff; search-provider rate
  limits need their own backoff tuning.

## Non-goals / follow-ups

- **No page-content fetching or crawling** — `web_search` returns
  `{title, url, snippet}` only; fetching a result's full page is `network`'s
  job (`http_request` directly, or a future `extract` provider).
- **No result caching in v1** — each call hits the provider. Caching is a
  deliberate non-goal (rate limits and freshness make it subtle); revisit
  only with a caller that needs it.
- **No image/news verticals** — v1 is web text results only. Provider
  verticals (`news`, `videos`, `images`) are a follow-up if the normalized
  shape grows to carry them.
- **No kernel special-casing for "search"** — an ordinary plugin like any
  other.
