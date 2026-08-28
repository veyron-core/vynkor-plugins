# gated-write

Reference high-risk plugin demonstrating the **D-09 confirmation gate** from
the [Vynkor Rust SDK](https://github.com/vynkor-core/vynkor-sdk-rust).

The risky operation — writing a file into a configured data dir — is split
into two actions:

| Action | Caller | Effect |
|---|---|---|
| `request_write` | any registered caller | stores the params as *pending*; the action spec is marked `requires_confirmation`; **nothing is written** |
| `confirm_write` | only callers on the confirm allowlist (default `device.*`) | executes the write with the params stored at request time |

This is the "permission separation" pattern from the Remote Devices plan
(§21.2 decision #2): the kernel stays dumb — a kernel gate would violate the
dumb-core rule — so enforcement lives entirely in the plugin, keyed on the
kernel-stamped `caller_plugin_id` (the kernel overwrites that field from the
real registered sender; it cannot be spoofed).

## Why split request/confirm?

The AI (or any restricted caller) can *ask* for a risky operation, but only
the user's device can *execute* it. A fully prompt-injected model can only
reach `request_write`; nothing happens until a human confirms. The confirming
caller cannot even swap the content — `confirm_write` executes the params
stored at request time, and pending requests expire (default 5 minutes).

## Config (env)

| Variable | Meaning |
|---|---|
| `GATED_WRITE_DATA_DIR` | **Required.** Directory confirmed writes land in. |
| `GATED_WRITE_CONFIRM_CALLERS` | Comma-separated plugin ids (or `prefix.*` globs) allowed to call `confirm_write`. Default `device.*` (every D-06 bridge mirror). |

Writes are confined to the data dir: absolute paths, `..` components,
symlinks and symlink-escapes are refused.

## Usage

```bash
# caller side — the AI requests:
curl -X POST http://host:8080/api/action \
  -H "Authorization: Bearer <ai-jwt>" \
  -d '{"action": "request_write", "params": {"path": "notes.txt", "content": "secret plan"}}'
# → {"pending_id": "pending-…"}

# the user's device confirms (caller_plugin_id matches device.*):
curl -X POST http://host:8080/api/action \
  -H "Authorization: Bearer <user-device-jwt>" \
  -d '{"action": "confirm_write", "params": {"pending_id": "pending-…"}}'
# → {"path": "/var/lib/vyn/gated-write/notes.txt", "bytes_written": 11}
```

With the SDK's one-liners:

```rust
use vynkor_sdk::confirmation_gate::{send_confirmation_request, send_confirmation};

let pending_id = send_confirmation_request(&mut client, "write", params).await?;
let resp = send_confirmation(&mut client, "write", &pending_id).await?;
```

The AI calling `confirm_write` gets `permission denied` (tested). See
`src/main.rs` for the plugin wiring and `vynkor-sdk`'s `confirmation_gate`
module for the reusable helper.
