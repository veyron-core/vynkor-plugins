# secrets plugin roadmap

Goal: make `secrets` the one blessed way any plugin stores credentials —
nothing keeps API keys/tokens in plaintext config, everything routes through
here so encryption at rest, per-caller isolation, and the `PERMISSION_SECRETS`
gate live in one place.

## Shipped (0.1.0)

- **Encrypted per-caller vaults** — one ChaCha20-Poly1305 vault file per
  kernel-stamped `caller_plugin_id` (`{SECRETS_PLUGIN_DATA_DIR}/{caller_id}.vault`),
  created `0600`, never shared, never a default namespace.
- **On-disk format** — `magic "VYNKORVLT"` + version byte + 12-byte nonce +
  AEAD ciphertext of the JSON secrets map; re-encrypted wholesale on every
  mutation.
- **Atomic writes** — temp file → fsync → rename → fsync dir. A torn write
  can never leave a half-written vault.
- **Tamper-loud** — a corrupted or key-mismatched vault fails decryption with
  an `ACTION_ERROR`; it is never silently reset or returned as empty.
- **Master key from env** — `SECRETS_PLUGIN_MASTER_KEY`, 32 bytes as 64 hex or
  44 base64 chars (generated via `openssl rand -hex 32`); missing/invalid key
  refuses startup. Losing the key loses the vault — no backdoor.
- **Four actions** — `secret_set`, `secret_get`, `secret_delete`,
  `secret_list`; all gated per-action by `PERMISSION_SECRETS` (Manifest v2
  data-driven, both caller and provider must hold it — no kernel change).
- **Size caps reject, never truncate** — name ≤ 256 B, value ≤ 256 KiB,
  name charset `[a-zA-Z0-9_.-]`.
- **Concurrent handler** — SDK `ConcurrentHandler` + `serve_concurrent`
  (vynkor-sdk ≥ 0.1.4), per-caller vault cached behind a lock, same pattern
  as `database`/`network`.
- **Verified against the live kernel** — `vynkor/tests/integration/
  test_secrets_plugin.rs` spawns the real binary against an in-process
  kernel: round-trip `secret_set`/`secret_get` over the real UDS wire,
  no-plaintext-in-vault assertion, and `ACTION_PERMISSION_DENY` for a caller
  without `PERMISSION_SECRETS`.

## Near-term (buildable now, no kernel changes)

- **Migrate shipped plugins onto `secrets`** — **done**: `ai`/`tts`/`stt`
  now resolve provider keys vault-first — `secret_get` against their own
  per-caller vault keyed by the env-var-style handle (`api_key_env`), with
  the plaintext config env var as fallback (vault wins). The env-var name
  stays the handle, so the per-caller permission story and the operator's
  mental model are intact; the allowlist
  (`AI_PLUGIN_ALLOWED_KEY_ENVS` etc.) still gates which handles a caller
  may reference.
- **`secret_delete` batch / `secret_get_many`** — only if a real consumer
  needs it; one-at-a-time is fine for the current scale.
- **Vault file locking across restarts** — single-writer assumption today
  (one plugin process per caller). A second kernel instance on the same data
  dir is undefined behavior; consider an exclusive flock on the vault file if
  that becomes reachable.

## Requires kernel/protocol changes

- (none known) — `PERMISSION_SECRETS = 15` is already in the wire protocol
  (v1.4+), `known_permissions()` accepts it, and the Manifest v2 per-action
  gate makes enforcement data-driven. Verified end-to-end by the integration
  tests above.

## Non-goals

- **Not a full KMS** — no key rotation, no remote signing, no HSM/TPM
  binding. The master key lives in the operator's env / secret store; the
  plugin is a symmetric-encryption vault, deliberately narrow.
- **No password-derived keys (Argon2 etc.)** — the master key is a raw
  32-byte key, not a passphrase. Equivalent entropy per byte with zero KDF
  surface; a future passphrase mode would be additive, not a migration.
- **No `secret_list` values** — names only. Values are only ever returned by
  exact-name `secret_get`.
- **No plaintext fallback** — if `SECRETS_PLUGIN_MASTER_KEY` is unset, the
  plugin refuses to start. It will never create an unencrypted vault.
