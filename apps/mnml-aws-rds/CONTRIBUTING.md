# Contributing to mnml-aws-codebuild

Thanks for taking a look! This repo is part of the [mnml integration family](https://mnml.sh/manual/integrations/community/) — a standalone AWS CodeBuild pull-request viewer that doubles as a hosted mnml pane.

## Two paths

**A. You want to fix a bug or add a AWS CodeBuild-specific feature here.** Open an issue or PR against this repo. See "Local development" below.

**B. You want a viewer for a different code-review system** (GitLab MRs, Gerrit, an internal review tool). **Fork this repo** and replace `src/aws-codebuild.rs` with your backend. The rest of the scaffold (`blit.rs`, `config.rs`, `ui.rs`, `keys.rs`, `app.rs`) is designed to be copy-pasted. See [Building integrations](https://mnml.sh/manual/integrations/building/) for the full guide. You don't owe anything back to this repo or to mnml — your fork can live under your own name.

## Project layout

```
src/
├── main.rs                # CLI + mode dispatch (TUI / --blit / --check)
├── app.rs                 # state — tabs, PR lists, selection, TabSpec::resolve
├── config.rs              # ~/.config/mnml-aws-codebuild.toml
├── auth.rs                # app-password loader from ~/.config/mnml-aws-codebuild/token
├── aws-codebuild.rs           # ← AWS CodeBuild REST v2 client (swap this when forking)
├── keys.rs                # action enum + key bindings
├── ui.rs                  # ratatui draw + crossterm loop
└── blit.rs                # tmnl-protocol over UDS — copied verbatim
```

`blit.rs` is shared verbatim across the family. Patches to `blit.rs` should land first in [`mnml-db-postgres`](https://github.com/chris-mclennan/mnml-db-postgres) and then be ported to the siblings.

## Local development

```sh
git clone https://github.com/chris-mclennan/mnml-aws-codebuild
cd mnml-aws-codebuild
cargo build
cargo test
cargo clippy --all-targets        # must be warning-free
cargo fmt                          # before committing
```

You'll need a AWS CodeBuild app password (free AWS CodeBuild account works) to test against the real API. Set up under <https://aws-codebuild.org/account/settings/app-passwords/> with **Pull requests: Read** + **Account: Read** scopes. Save it to `~/.config/mnml-aws-codebuild/token` and run `cargo run -- --check`.

## PR conventions

- One commit per logical change is fine; squash on merge is fine too.
- Commit messages: short imperative subject (≤72 chars), optional body explaining "why".
- Add a unit test for any tab-resolution or config-parsing change (`src/app.rs` and `src/config.rs` have examples).
- `cargo clippy --all-targets` and `cargo fmt --check` must be clean.

## License + ownership

MIT. Contributions are accepted under the same license. No copyright assignment required; you keep authorship of your changes.
