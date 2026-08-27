# contacts plugin

vCard-ish contact store as a thin schema over `database`.

| Action | Params | Result |
|---|---|---|
| `contact_create` | `{name, email?, phone?, notes?, tags?[]}` | `{id, contact}` |
| `contact_get` | `{id}` | `{found, contact?}` |
| `contact_list` | `{query?, tag?, limit?, offset?}` | `{contacts: [...], total}` |
| `contact_update` | `{id, name?, email?, phone?, notes?, tags?}` | `{updated, contact}` |
| `contact_delete` | `{id}` | `{deleted}` |

`query` is case-insensitive substring over `name/email/phone/notes`. Publishes `plugin.contacts.changed` `{op: created|updated|deleted, id}` best-effort after response, same contract as `notes`.

## Validation

- `name` 1..256 trimmed, required
- `email` 0..256, light `user@host.domain` check when non-empty
- `phone` 0..64
- `notes` 0..4096
- `tags` ≤32 unique ≤64 each, trimmed/deduped

## Config

`CONTACTS_PLUGIN_DB_TIMEOUT_MS` default 5000.

## Testing

`cargo test` — fake kernel over `UnixStream::pair`.
