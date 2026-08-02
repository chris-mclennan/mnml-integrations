# Contributing to mnml-tracker-jira

Thanks for taking a look! This repo is part of the [mnml integration family](https://mnml.sh/manual/integrations/community/) — a standalone Jira ticket viewer that doubles as a hosted mnml pane.

## Two paths

**A. You want to fix a bug or add a Jira-specific feature here.** Open an issue or PR against this repo. See "Local development" below.

**B. You want a viewer for a different ticket system** (Shortcut, Pivotal Tracker, an internal tracker). **Fork this repo** and replace `src/jira.rs` with your backend (the tab-resolution logic — literal JQL / current/next release — also lives in `jira.rs`). The rest of the scaffold (`blit.rs`, `config.rs`, `ui.rs`, `keys.rs`, `app.rs`) is designed to be copy-pasted. See [Building integrations](https://mnml.sh/manual/integrations/building/) for the full guide. You don't owe anything back to this repo or to mnml — your fork can live under your own name.

## Project layout

```
src/
├── main.rs                # CLI + mode dispatch (TUI / --blit / --check)
├── app.rs                 # state — tabs, ticket lists, selection
├── config.rs              # ~/.config/mnml-tracker-jira.toml
├── jira.rs                # ← Jira REST client + tab resolution (swap this when forking)
├── auth.rs                # token loading from ~/.config/mnml-tracker-jira/token
├── keys.rs                # action enum + key bindings
├── ui.rs                  # ratatui draw + crossterm loop
└── blit.rs                # tmnl-protocol over UDS — copied verbatim
```

This is the cleanest fork target for **ticket-system viewers** — the tabbed list shape, periodic refresh, and open-in-browser pattern carry directly to most issue trackers. The DB siblings have a query-buffer-and-results shape instead, which is a different fork.

`blit.rs` is shared verbatim across the family.

## Local development

```sh
git clone https://github.com/chris-mclennan/mnml-tracker-jira
cd mnml-tracker-jira
cargo build
cargo test
cargo clippy --all-targets        # must be warning-free
cargo fmt                          # before committing
```

You'll need a Jira instance to test against. The free **Atlassian Cloud** tier works (create a sandbox site at `<your-name>.atlassian.net`). Save your API token to `~/.config/mnml-tracker-jira/token` and run `cargo run -- --check` to verify.

## PR conventions

- One commit per logical change is fine; squash on merge is fine too.
- Commit messages: short imperative subject (≤72 chars), optional body explaining "why".
- Add a unit test for any tab-resolution or config-parsing change.
- `cargo clippy --all-targets` and `cargo fmt --check` must be clean.

## License + ownership

MIT. Contributions are accepted under the same license. No copyright assignment required; you keep authorship of your changes.
