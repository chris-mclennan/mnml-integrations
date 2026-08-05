# mnml-meta-stats

Download-stats report for the mnml crate family.

Pulls crates.io + GitHub Releases APIs for every `mnml-*` package and
prints a plain-text table + 30-day sparklines. Runs standalone or as
an mnml-hosted Pty pane.

## Install

```
cargo install mnml-meta-stats
mnml-meta-stats --install
```

The `--install` step writes `~/.config/mnml/integrations/meta_stats.toml`
so a rail chip + `:meta_stats.open` palette command show up on next
mnml start (or `:integrations.refresh`).

## Use

```
mnml-meta-stats           # pretty table
mnml-meta-stats --json    # machine-readable
mnml-meta-stats --uninstall
```

All data is public — no auth needed. The tool uses the `mnml-integration`
keyword to enumerate crates, so any community-published `mnml-*` crate
that tags itself with that keyword shows up automatically.
