# Contributing to mnml-forge-github

Thanks for taking a look! This repo is part of the [mnml integration family](https://mnml.sh/manual/integrations/community/) — a standalone GitHub Issues + Pull Requests viewer that doubles as a hosted mnml pane.

## Two paths

**A. You want to fix a bug or add a GitHub-specific feature here.** Open an issue or PR against this repo. See "Local development" below.

**B. You want a viewer for a different ticket system** (GitLab, Shortcut, an internal tracker). **Fork this repo** and replace `src/github.rs` with your backend. The rest of the scaffold (`blit.rs`, `config.rs`, `ui.rs`, `keys.rs`, `app.rs`) is designed to be copy-pasted. See [Building integrations](https://mnml.sh/manual/integrations/building/) for the full guide. You don't owe anything back to this repo or to mnml — your fork can live under your own name.

## Project layout

```
src/
├── main.rs                # CLI + mode dispatch (TUI / --blit / --check)
├── app.rs                 # state — tabs, ticket lists, selection
├── config.rs              # ~/.config/mnml-forge-github.toml
├── github.rs              # ← GitHub REST client (swap this when forking)
├── keys.rs                # action enum + key bindings
├── ui.rs                  # ratatui draw + crossterm loop
└── blit.rs                # tmnl-protocol over UDS — copied verbatim
```

`blit.rs` is shared verbatim across the family.

## Local development

```sh
git clone https://github.com/chris-mclennan/mnml-forge-github
cd mnml-forge-github
cargo build
cargo test
cargo clippy --all-targets        # must be warning-free
cargo fmt                          # before committing
```

You'll need a GitHub personal access token to test against the real API. A classic PAT with `repo` (and `read:org` if you want org-level tabs) is sufficient. Save it to `~/.config/mnml-forge-github/token`, then run `cargo run -- --check`.

## PR conventions

- One commit per logical change is fine; squash on merge is fine too.
- Commit messages: short imperative subject (≤72 chars), optional body explaining "why".
- Add a unit test for any config-parsing or query-building change.
- `cargo clippy --all-targets` and `cargo fmt --check` must be clean.

## License + ownership

MIT. Contributions are accepted under the same license. No copyright assignment required; you keep authorship of your changes.
