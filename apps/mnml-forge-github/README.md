# mnml-forge-github

GitHub Issues + PRs viewer for [mnml](https://mnml.sh) — terminal TUI
with configurable tabs backed by GitHub's issue-search API. Sibling
to [mnml-tickets-jira](https://github.com/chris-mclennan/mnml-tickets-jira)
and [mnml-tickets-linear](https://github.com/chris-mclennan/mnml-tickets-linear);
same shape, same blit.rs, just swapping the API.

```
┌─ github ─────────────────────────────────────────────────────────┐
│ ▸1.Mine (12)  2.Reported (5)  3.PRs (8)                          │
└──────────────────────────────────────────────────────────────────┘
┌─ Mine ───────────────────────────────────────────────────────────┐
│ KIND   REPO                  KEY    STATE  AUTHOR        UPDATED   TITLE
│ issue  chris-mclennan/mnml   #128   open   chris-mclennan 2026-06-03 Fix…
│ PR     chris-mclennan/tmnl   #45    open   chris-mclennan 2026-06-02 Add…
│ …                                                                │
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-forge-github mnml-forge-github
mnml-forge-github --install
```

(Homebrew tap + binary releases follow once the binary stabilises.)

## Setup

1. **Generate a GitHub PAT** at <https://github.com/settings/tokens>.
   Classic with `repo` scope works (or `public_repo` if you only care
   about public repos); fine-grained PATs need Issues + Pull requests
   read on the repos you want to query.

2. **Save the token** to `~/.config/mnml-forge-github/token`
   (`chmod 600`).

3. **Run once** to scaffold the config:
   ```sh
   mnml-forge-github
   ```
   This writes `~/.config/mnml-forge-github.toml` and exits with
   instructions. Edit the `[[tabs]]` list to taste.

4. **Re-run** — the TUI launches with your configured tabs.

5. **Verify** the resolved config + auth state:
   ```sh
   mnml-forge-github --check
   ```

## Tabs

Each `[[tabs]]` entry is one tab with a GitHub issue-search query —
same syntax as the web UI's search box.

```toml
# Across all repos: issues assigned to you, most recent first.
[[tabs]]
name = "Mine"
query = "is:open is:issue assignee:@me"

# Open PRs you're involved in (review-requested, assigned, author).
[[tabs]]
name = "PRs"
query = "is:open is:pr involves:@me"

# Repo-scoped — open bugs on a specific repo.
[[tabs]]
name = "mnml bugs"
query = "repo:chris-mclennan/mnml is:open is:issue label:bug"
```

Reference: <https://docs.github.com/en/search-github/searching-on-github/searching-issues-and-pull-requests>

## Keys

| Chord          | Action                                       |
|----------------|----------------------------------------------|
| `1`-`9`        | Switch to that tab                           |
| `Tab` / `BackTab` | Cycle tabs forward / back                 |
| `↑` / `k`, `↓` / `j` | Move selection                         |
| `PgUp` / `PgDn` | Jump 10 rows                                |
| `g` / `G`      | Top / bottom                                 |
| `Enter` / `o`  | Open focused issue/PR in browser             |
| `r`            | Refresh active tab                           |
| `q` / `Esc` / `Ctrl+C` | Quit                                |

## Status & roadmap

**v0.1 (this release):**
- Standalone TUI
- Configurable tabs via GitHub issue-search queries
- 1-9 tab switching · ↑↓ navigation · open-in-browser · refresh
- Blit mode (`--blit <socket>`) so mnml/tmnl can host as a pane
- Differentiates issues vs PRs (KIND column + magenta/cyan styling)

**Planned (paralleling mnml-tickets-jira's v0.2):**
- Right-half detail panel (body + comments + reviews)
- Filter editor overlay (`/`) on top of the per-tab query
- Status transition (close/reopen/merge)
- Watcher (subscribe) toggle
- Comment posting
- Bulk operations across selected rows
- Inline-edit labels / assignees

## License

MIT.
