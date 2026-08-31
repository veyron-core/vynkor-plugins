# Contributing to vynkor-plugins

## Adding a new plugin

1. Fork `vynkor-core/vynkor-plugins` and create a branch.
2. Copy a minimal plugin (e.g. `plugins/ping-pong-rs/`) to `plugins/<your-slug>/`.
3. Fill in:
   - `README.md` — what the plugin does, actions/events, permissions
   - `Cargo.toml` / `pyproject.toml` — add `vynkor-sdk` dependency
   - `manifest` — `plugin_id`, `actions`, `permissions` (see `docs/PLUGIN_AUTHORING.md` for the `Plugin` trait and single-reader loop)
4. Test with a fake kernel: `cargo test` (see `docs/PLUGIN_AUTHORING.md` § fake-kernel harness) and on a real kernel with `sandbox: false` first.
5. Add an entry to `registry.json` (`v2` format: `slug`, `name`, `description`, `category`, `tags`, `versions`, `latest`).

## Registry

- `registry.json` is the source `vynm search/install` queries. Keep `meta.lastUpdated` and `revoked` at the root.
- Each `slug` entry must have `versions[]` with `sha256`, `url`, and `compat.kernel`.

## Style

- No `as any` / `@ts-ignore` / `unwrap()` without reason. Use `Result` + typed errors.
- Explain *why* in comments, not what.
- Run `cargo fmt && cargo clippy -- -D warnings` before PR.

## Questions

Open a Discussion or an Issue with label `plugin`.
