# github plugin

Outbound-only GitHub API for voice / agent — issues, PRs, and Actions runs over `network`'s gated `http_request`.

No inbound webhook, no local clone. The plugin is a caller of `network` (T-19), so it declares `PERMISSION_NETWORK` and every GitHub action maps to one `network` `http_request` call under the hood.

## Actions

| Action | Params | Result |
|---|---|---|
| `gh_list_issues` | `{repo, state?("open"/"closed"/"all"), limit?1..100, pat_env?}` | `{issues: [...]}` |
| `gh_list_prs` | `{repo, state?("open"/"closed"/"all"), limit?1..100, pat_env?}` | `{prs: [...]}` |
| `gh_create_issue` | `{repo, title 1..256, body?≤10000, pat_env?}` | `{issue: {...}}` |
| `gh_list_runs` | `{repo, branch?, limit?1..100, pat_env?}` | `{runs: [...]}` |

- `repo` is `owner/repo` (trimmed, `must contain '/'`, ≤200 bytes). `state` defaults to `open`, `limit` defaults vary (`20` for issues/PRs, `10` for runs), all clamped server-side.
- Pagination is `per_page` only (no cursor) — ask the agent to page with successive calls if needed.
- Every action authenticates with a GitHub PAT — never in plaintext config.

## Auth: vault-first PAT + allowlisted env fallback

Resolution order for the PAT:

1. `secret_get {name: pat_env}` via `secrets` plugin — vault wins when present.
2. Fallback to process env `pat_env` (the same name).
3. If neither exists → `ERR_GITHUB_NO_PAT`.

`pat_env` is the env-var *name* that holds the token, not the token itself (e.g. `"GITHUB_TOKEN"`). Caller may pass `pat_env`, otherwise defaults to `GITHUB_PLUGIN_PAT_ENV` (default `GITHUB_TOKEN`).

The operator must allowlist which env names are readable at all via `GITHUB_PLUGIN_ALLOWED_PAT_ENVS` (comma-separated). If set and non-empty, a call referencing a name outside the list is denied before any vault/env lookup — default-deny. Example: `GITHUB_PLUGIN_ALLOWED_PAT_ENVS=GITHUB_TOKEN,GH_RO_PAT`.

The token is sent as `Authorization: Bearer <pat>` + `Accept: application/vnd.github+json` + `X-GitHub-Api-Version: 2022-11-28`, over `network` → `GITHUB_API = https://api.github.com`.

## Config

| Env | Default | Meaning |
|---|---|---|
| `GITHUB_PLUGIN_ALLOWED_PAT_ENVS` | _(empty)_ | Comma-separated allowlist of env var names that may hold a PAT. Empty → historically allow-all (set it in production). |
| `GITHUB_PLUGIN_PAT_ENV` | `GITHUB_TOKEN` | Default `pat_env` when the caller omits it |
| `GITHUB_PLUGIN_FETCH_TIMEOUT_MS` | `10000` | `network` HTTP timeout per GitHub call |

## Examples

```json
// list open issues
{"repo":"vynkor-core/vynkor-plugins","state":"open","limit":5}
→ {"issues": [{"number":12,"title":"...","html_url":"..."}, ...]}

// create issue (vault must hold GITHUB_TOKEN or pat_env points to an allowlisted env)
{"repo":"vynkor-core/vynkor","title":"voice: triage inbox","body":"from vynkor agent"}
→ {"issue": {"number": 84, "html_url": "https://github.com/..." }}

// PRs + runs
{"repo":"vynkor-core/vynkor","state":"open","limit":10} → {"prs": [...]}
{"repo":"vynkor-core/vynkor","branch":"main","limit":5} → {"runs": [{"id": 123, "status":"completed", "conclusion":"success"}, ...]}
```

## Testing

`cargo test` — 4 tests: `rejects_bad_repo`, `list_issues_defaults`, `create_issue_ok`, and `vault_allowlist` (pure allowlist unit). No live network; the fake kernel never leaves `UnixStream::pair`.
