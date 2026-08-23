# email plugin

SMTP email sending for vynkor plugins. Exposes one real action, `email_send`,
plus a v0.2-stub `email_list`. Resolves the SMTP password vault-first (via the
`secrets` plugin's `secret_get` action), then sends with `lettre` over
STARTTLS/submission. See `ROADMAP.md` for the design rationale.

## Operator note

`email` declares one kernel permission — `secrets` (`plugin.json`:
`"permissions": ["secrets"]`) — because it invokes the `secrets` plugin's
gated `secret_get` action, and the kernel's anti-laundering check (T-19)
requires callers of a gated action to hold its permission too (Manifest v2:
per-action `permission` on `secret_get`). It declares **no**
`PERMISSION_NETWORK`: unlike `search`/`ai`/`tts`/`stt`, `email` opens its own
SMTP socket (lettre) rather than routing through `network`'s `http_request`,
so it must be run with `sandbox: false` (real egress). `secrets` must also be
registered and running for vault-first credentials.

## Action: `email_send`

Request (`ActionRequest.params_json`):

```json
{
  "to": "user@example.com",
  "from": "vynkor@example.com",
  "subject": "Hello",
  "body": "Hello there",
  "is_html": false,
  "credentials_env": "EMAIL_SMTP_PASS",
  "smtp_host": "smtp.example.com",
  "smtp_port": 587,
  "smtp_user": "smtp-login",
  "timeout_ms": 30000
}
```

- `to` — required. Must contain an `@` and a `.` after the `@`.
- `from` — optional. Defaults to `vynkor@localhost`, or the `smtp_user` when
  that is set. Never a literal password.
- `subject` — required, 1-200 chars.
- `body` — required, non-empty, capped at 10000 chars.
- `is_html` — optional, default `false`; `true` sends the body as `text/html`.
- `credentials_env` — required. Name under which the `email` process resolves
  the SMTP password at call time, never a literal password. Resolution is
  vault-first: `email` asks the `secrets` plugin's vault for a secret stored
  under that exact name (`secret_set {"name":"...","value":"..."}` by the
  operator), and falls back to the environment variable of the same name only
  when the vault has no non-empty value. The vault wins when both exist. Must
  appear in the operator's `EMAIL_PLUGIN_ALLOWED_CRED_ENVS` allowlist (see
  "Configuration") — otherwise a caller could name *any* secret/env var the
  process has, not just the SMTP password, and exfiltrate it. Not
  allowlisted, or unset in both sources → `ACTION_ERROR`; the password value
  never appears in any error string.
- `smtp_host` — optional, default `localhost`.
- `smtp_port` — optional, default `587` (SMTP submission).
- `smtp_user` — optional SMTP auth username, default the `from` address.
- `timeout_ms` — optional, default and cap `30000` (SMTP connection timeout).

Response (`ActionResponse.data_json`) on success:

```json
{ "message_id": "stub-1710000000000", "to": "user@example.com", "subject": "Hello", "stubbed": false }
```

`stubbed` is `true` (plus `smtp_host`/`smtp_port` debug fields) when the
process runs with `EMAIL_PLUGIN_SMTP_STUB=true` and the real SMTP send was
skipped. Errors → `ACTION_ERROR` with a human-readable message; the resolved
password never appears in any error string or log line.

## Configuration

`email` reads no config file itself. The only configuration is environment
variables set in the kernel's `config.yaml`, under this plugin's `env:` list —
see `config.example.yaml`. The SMTP password is resolved vault-first (see
above), with the plugin's own env vars as fallback.

`EMAIL_PLUGIN_ALLOWED_CRED_ENVS` is **required**: a comma-separated,
exact-match allowlist of every env var name a caller's `credentials_env` may
reference. Default-deny — omit it and every `email_send` request is rejected.

`EMAIL_PLUGIN_SMTP_STUB=true` switches sends into stub mode (no network,
clearly-marked success) — for smoke-tests and the offline test suite.

```yaml
plugins:
  - id: email
    binary: /opt/plugins/email
    sandbox: false               # email opens its own SMTP socket
    env:
      - EMAIL_PLUGIN_ALLOWED_CRED_ENVS=EMAIL_SMTP_PASS
      - EMAIL_SMTP_PASS=<smtp password>   # or store it in the secrets vault instead
      # - EMAIL_PLUGIN_SMTP_STUB=true     # smoke-test mode
```

## Testing

`cargo test` — no live network. Request parsing and the allowlist are
unit-tested, and a fake-kernel integration test drives the full handler
end-to-end over `UnixStream::pair` (a shim answers `PluginRegister` and
`secret_get`), with `EMAIL_PLUGIN_SMTP_STUB=true` keeping the SMTP send
offline. It asserts the vault-first resolution (one `secret_get` for the
allowlisted handle), the stub success shape, and that no error path leaks the
resolved password.
