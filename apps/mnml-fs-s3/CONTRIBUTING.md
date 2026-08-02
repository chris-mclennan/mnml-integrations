# Contributing to mnml-fs-s3

Thanks for taking a look! This repo is part of the [mnml integration family](https://mnml.sh/manual/integrations/community/) — a standalone Amazon S3 browser that doubles as a hosted mnml pane.

## Two paths

**A. You want to fix a bug or add an S3-specific feature here.** Open an issue or PR against this repo. See "Local development" below.

**B. You want a viewer for a different cloud filesystem** (Google Cloud Storage, Azure Blob Storage, an internal object store). **Fork this repo** and replace `src/s3.rs` with your backend (the `aws` CLI shell-outs — `list_prefix`, `download`, `upload`, `delete`, `presign` — are the surface to swap). The rest of the scaffold (`blit.rs`, `config.rs`, `ui.rs`, `keys.rs`, `app.rs`) is designed to be copy-pasted. See [Building integrations](https://mnml.sh/manual/integrations/building/) for the full guide. You don't owe anything back to this repo or to mnml — your fork can live under your own name.

This is the cleanest reference for **filesystem-shape viewers** — the tabbed bucket / breadcrumb-navigation / download-to-cache pattern carries directly to most object stores.

## Project layout

```
src/
├── main.rs        # CLI + mode dispatch (TUI / --blit / --check)
├── app.rs         # State: bucket tabs, prefix stack, selection
├── config.rs      # ~/.config/mnml-fs-s3.toml
├── s3.rs          # ← `aws s3` / `aws s3api` shell-outs (swap this when forking)
├── clipboard.rs   # OS clipboard wrapper (verbatim from forge siblings)
├── keys.rs        # Action enum + key bindings
├── ui.rs          # ratatui draw + crossterm loop
└── blit.rs        # tmnl-protocol over UDS — copied verbatim
```

`blit.rs` is shared verbatim across the family.

## Local development

```sh
git clone https://github.com/chris-mclennan/mnml-fs-s3
cd mnml-fs-s3
cargo build
cargo test
cargo clippy --all-targets        # must be warning-free
cargo fmt                          # before committing
```

You'll need:
- The AWS CLI on `$PATH` (`aws --version` should work)
- An AWS account with at least one S3 bucket you have list access to
- `aws configure` (or env vars / SSO / instance role) set up

Run `cargo run -- --check` to verify the config + AWS CLI are wired up correctly.

## PR conventions

- One commit per logical change is fine; squash on merge is fine too.
- Commit messages: short imperative subject (≤72 chars), optional body explaining "why".
- Add a unit test for any config-parsing or s3-shell-out parsing change.
- `cargo clippy --all-targets` and `cargo fmt --check` must be clean.

## License + ownership

MIT. Contributions are accepted under the same license. No copyright assignment required; you keep authorship of your changes.
