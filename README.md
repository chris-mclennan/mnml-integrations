# mnml-integrations

The monorepo for mnml's ecosystem — every mnml **app** (compiled sibling
that extends mnml with its own binary + pane) and every mnml **launcher**
(TOML-only descriptor that turns an external CLI into an mnml chip).

## Layout

```
mnml-integrations/
├── Cargo.toml              cargo workspace root (built-in cargo build --workspace)
├── apps/                   compiled siblings — each a crate + binary
│   ├── mnml-aws-amplify/
│   ├── mnml-msg-slack/
│   └── …
└── launchers/              TOML-only launchers
    ├── htop.toml
    ├── iftop.toml
    ├── btop.toml
    ├── mixr.toml
    └── claude-multi.toml
```

## What's an app vs a launcher

- **App** — its own binary (`mnml-aws-amplify`, etc.). Runs as a subprocess
  in an mnml Pty pane. Ships an `install.rs` that writes a manifest to
  `~/.config/mnml/integrations/<id>.toml` on install. Discovered by the
  mnml marketplace via crates.io keyword `mnml-integration`.

- **Launcher** — a TOML file. No code, no binary, no compilation. Turns
  an external CLI (`htop`, `code`, `docker`, `ssh`, whatever) into an
  mnml chip with a glyph, label, color, and one or more parameterized
  actions. Discovered by the mnml marketplace via GitHub contents API
  scanning `mnml-integrations/launchers/*.toml`.

## Launcher schema

Every launcher TOML follows the same shape as an installed integration
manifest, minus the `binary` field:

```toml
id = "htop"
label = "htop"
description = "Interactive process viewer"
category = "system"

[chip]
glyph = "\u{F0AF5}"
fallback = "H"
color = "green"
enabled = false
in_palette_bar = false

[[commands]]
id = "htop.open"
title = "htop: open"
group = "system"
keys = ["<leader>iH"]
run = ":term htop"
```

Action `run` strings support template substitution via `{{name}}` tokens.
See `src/launcher_template.rs` in mnml core for the supported variables
(`{{workspace}}`, `{{current_file}}`, `{{cursor_line}}`, etc.).

## Contributing a launcher

1. Add a `.toml` file under `launchers/` — one per launcher.
2. Verify it parses: `cargo run -p mnml -- launcher validate launchers/your-launcher.toml`
   *(TODO once the validate command lands)*
3. Open a PR. Reviewer checks the metadata is accurate and the `run`
   commands are safe (no destructive defaults, no exfiltration).

## Contributing an app

Same as any sibling repo before the monorepo — see `apps/<any>/README.md`
for the sibling authoring guide. Rough shape:

- `Cargo.toml` with `mnml-bridge = "0.4"` as a dep
- `src/install.rs` writing an `IntegrationSpec` via `install_integration`
- `src/main.rs` with a `--install` and `--uninstall` subcommand
- Its own `README.md`, own tests, own CI

Publish to crates.io with the `mnml-integration` keyword so mnml's
marketplace discovers it.

## Trust model

This repo is the **reference collection** — curated by @chris-mclennan.
It's one source among many:

- Third parties can publish their own apps to crates.io independently.
- Third parties can run their own launcher catalogs in their own repos.
- Users configure `[marketplace]` in mnml to point at any combination
  of sources. Removing this repo from the config disables the reference
  catalog with no impact on other sources.

Nothing in mnml core hardcodes this repo's URL — federation from day one.
