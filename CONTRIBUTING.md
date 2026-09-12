# Contributing

Thanks for helping improve Discodex.

## Before you start

- Search existing issues before opening a duplicate.
- Keep provider adapters privacy-conscious: parse only the metadata required for Rich Presence.
- Avoid adding dependencies when the Windows/Rust standard stack already provides the needed functionality.
- Keep changes focused and reviewable.

## Local setup

```powershell
git clone https://github.com/braces157/discodex.git
cd discodex
cargo test
```

Before opening a pull request, run:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## Adding provider support

Focused provider recognition generally belongs in `src/provider.rs`.

For a rich session adapter:

1. Add the session root and log matcher in `src/provider.rs`.
2. Parse only required event metadata in `src/events.rs`.
3. Normalize tool activity into safe labels instead of forwarding raw tool content.
4. Add tests proving that prompts/responses/tool output do not enter the resulting activity state.
5. Update `docs/PROVIDERS.md` and the support table in `README.md`.

Do not commit real session logs, tokens, private prompts, account data, or screenshots containing sensitive conversations.

## Pull requests

A good pull request explains:

- The behavior being changed
- Why the change is needed
- How it was verified
- Any provider-version assumptions or limitations

Small changes are easier to review and maintain.

