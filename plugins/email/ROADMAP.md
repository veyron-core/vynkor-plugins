# email plugin roadmap

Goal: give any vynkor plugin one blessed path to send an SMTP email, with the
password in one place (the `secrets` vault) instead of every plugin rolling its
own SMTP client.

## Decision: own SMTP socket, secrets for the password

`email` opens its own SMTP connection via `lettre` — it does **not** route
through `network`'s `http_request`, because SMTP is not HTTP. For the
credential it is vault-first, identical to `search`/`ai`/`tts`/`stt`: it calls
the kernel-routed `secret_get` action (owned by the `secrets` plugin) via
`VynkorClient::send_action`, with the process environment as fallback. Because
`secret_get` is gated by `PERMISSION_SECRETS`, and the kernel's
anti-laundering check (T-19) requires the *caller* to hold a gated action's
permission as well as the provider, `email` declares `"permissions":
["secrets"]` (Manifest v2 per-action `permission` on `secret_get`).

`email` declares **no** `PERMISSION_NETWORK` — it does not call
`network`'s `http_request`, so it runs with `sandbox: false` (real egress).

## Naming

Plugin id: `email`. Binary: `email`. Env-var prefix `EMAIL_PLUGIN_*` keeps the
established spelling (the vynkor rename doesn't touch protocol/config
surfaces).

## v0.1 (shipped)

- One real action, `email_send`:
  - Fields `to` (required), `from`/`subject`/`body`/`is_html`,
    `credentials_env` (required, allowlisted via
    `EMAIL_PLUGIN_ALLOWED_CRED_ENVS`), `smtp_host`/`smtp_port`/`smtp_user`,
    `timeout_ms`.
  - Vault-first password resolution (`src/key_resolve.rs`, adapted from
    `search`): `secret_get` against `secrets`, env-var fallback of the same
    name; vault wins; resolved per request. The value never appears in an
    error string or log line.
  - `lettre` 0.11 with `tokio1-native-tls`: `AsyncSmtpTransport::relay` +
    `Credentials`, STARTTLS/submission. HTML vs plain via `is_html`.
  - `EMAIL_PLUGIN_SMTP_STUB=true` stub mode: skips the real send, returns a
    clearly-marked success — keeps the test suite offline and enables
    operator smoke-tests.
- `email_list` declared but a stub (`ACTION_ERROR: not implemented in v0.1`).
- Strict parse-time validation: `to` must contain `@` and a `.` after it,
  `subject` 1-200 chars, `body` non-empty (≤10000), `credentials_env`
  non-empty, `smtp_port` non-zero.
- Testing: `request.rs` unit tests (validation + allowlist) and a fake-kernel
  `UnixStream::pair` integration test driving the full handler end to end.

## v0.2 (planned)

- **`email_list`** — outbox listing backed by `database` (or the `secrets`
  plugin for a sent-log), so callers can query what was sent. Deferred until
  a caller needs it.
- **Attachments** — MIME multipart with base64 bodies. Needs `lettre`'s
  `builder`/mime support and an operator cap on total payload size.
- **Reply-To / CC / BCC** — additional recipient fields once a caller asks.
- **Provider abstraction** — optional: a trait over `lettre` vs a
  `network`-routed HTTP email API (Resend/Postmark/SendGrid), mirroring
  `search`'s provider adapters, if an HTTP-only deployment is needed.
- **Retry/backoff** — mirror `network`'s retry-on-429-equivalent (SMTP 421/450)
  with per-account backoff tuning.

## Non-goals / follow-ups

- **No inbound email (IMAP/POP3)** — `email` sends only.
- **No templating** — the caller builds the subject/body; `email` transmits
  them verbatim.
- **No secret logging** — the resolved password is never logged, cached to
  disk, or embedded in any error string; the stub response carries only
  non-secret debug fields.
- **No kernel special-casing for "email"** — an ordinary plugin like any other.
