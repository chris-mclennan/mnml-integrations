# mnml-tracker-jira

Jira ticket viewer for [mnml](https://mnml.sh) — terminal TUI with
configurable tabs (JQL queries or auto-resolved release `fixVersion`s),
a right-half detail panel, status transitions, inline assignee /
fixVersion editing, comment posting, and bulk operations across
selected rows. Runs standalone in any terminal or as a hosted
mnml pane via the blit protocol.

```
┌─ tickets ────────────────────────────────────────────────────────┐
│ ▸1.Testing (12)  2.Current (47)  3.Next (8)  4.Mobile (3)  5.Mine │
└────────┬─────────────────────────────────────────────────────────┘
┌─ Testing─┼──────────────┐┌─ TE-1234 ★ watching (4 total) ──────┐
│ KEY      │ STATUS  …    ││ Bug · Highest · @chrismclennan       │
│ TE-1234▸│ Testing …    ││ fixVersion: 6.4 · reporter: andrew    │
│ TE-1235  │ Testing …    ││                                       │
│ TE-1241  │ Testing …    ││ When the bufferline drops a tab on   │
│ TE-1244  │ Testing …    ││ window resize the next render panic… │
│ …                       ││                                       │
│                         ││ comments (3, most-recent first):     │
│                         ││  ▸ chrismclennan · 2026-06-02         │
│                         ││    repro on 0.1.2 too, fix forthcoming│
└─────────────────────────┘└──────────────────────────────────────┘
  d toggle detail · t transition · / filter · w watch · c comment · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-tracker-jira mnml-tracker-jira
mnml-tracker-jira --install
```

(Homebrew tap + binary releases will follow once the binary stabilises.)

## Setup

1. **Get a Jira API token**:
   <https://id.atlassian.com/manage-profile/security/api-tokens>

2. **Save the token** to `~/.config/mnml-tracker-jira/token`
   (`chmod 600`).

3. **Run once** to scaffold the config template:
   ```sh
   mnml-tracker-jira
   ```
   This writes `~/.config/mnml-tracker-jira.toml` and exits with
   instructions. Edit `jira_url`, `email`, and the `[[tabs]]` list.

4. **Re-run** — the TUI launches with your configured tabs.

5. **Verify** the resolved config + auth state:
   ```sh
   mnml-tracker-jira --check
   ```

## Tabs

Each `[[tabs]]` entry is one tab. Either:

```toml
# Literal JQL — full control, you maintain version strings.
[[tabs]]
name = "Mine"
jql  = "reporter = currentUser() ORDER BY updated DESC"
```

…or auto-resolved against the project's release list:

```toml
# Auto-resolve: first unreleased fixVersion of project TE.
[[tabs]]
name    = "Current"
mode    = "current_release"
project = "TE"

# Auto-resolve: second unreleased fixVersion of TE, filtered to the
# Mobile component.
[[tabs]]
name      = "Mobile"
mode      = "next_release"
project   = "TE"
component = "Mobile"
```

Modes:

| `mode`              | Resolves to                                   |
|---------------------|-----------------------------------------------|
| `current_release`   | Earliest unreleased fixVersion of `project`  |
| `next_release`      | Second-earliest unreleased fixVersion (falls back to current if only one exists) |

## Keys

| Chord                  | Action                                             |
|------------------------|----------------------------------------------------|
| `1`-`9`                | Switch to that tab                                 |
| `Tab` / `BackTab`      | Cycle tabs forward / back                          |
| `↑` / `k`, `↓` / `j`   | Move selection                                     |
| `PgUp` / `PgDn`        | Jump 10 rows                                       |
| `g` / `G`              | Top / bottom                                       |
| `Enter` / `o`          | Open focused ticket in browser                     |
| `d`                    | Toggle right-half ticket detail panel              |
| `Ctrl+u` / `Ctrl+d`    | Scroll detail panel up / down (when open)          |
| `/`                    | Open filter editor (substring match)               |
| `t`                    | Open status transition picker for focused ticket (operates on multi-selection if non-empty) |
| `a`                    | Open assignee picker (operates on multi-selection if non-empty) |
| `f`                    | Open fixVersion picker (operates on multi-selection if non-empty) |
| `c`                    | Open inline comment editor (detail panel must be open) — `Ctrl+S` posts, `Esc` cancels |
| `w`                    | Toggle watch on focused ticket                     |
| `Space`                | Toggle focused row in multi-selection set          |
| `r`                    | Refresh active tab (+ detail if open)              |
| `Esc`                  | Cascade: clear selection → clear filter → close detail → quit |
| `q` / `Ctrl+C`         | Quit                                               |

### Multi-selection + bulk ops

`Space` toggles the focused row into a per-tab selection set, marked
visually in the leftmost column. With at least one row selected, `t`
runs the chosen transition on every selected ticket (parallel, with
error tally); `a` / `f` set the chosen assignee / fixVersion on every
selected ticket the same way. Selection clears on tab switch and after
a successful bulk op.

### Detail panel

`d` opens a right-half panel for the focused ticket: type / status /
priority / assignee / reporter / fixVersion header (with watcher chip),
then description, then up to the last 10 comments (most-recent first).
The narrative content (description + comments) is lazy-loaded on first
focus and cached per-issue key — arrow-keying through a long list only
fetches once per ticket.

`r` while the detail panel is open invalidates the cached detail for
the focused ticket and re-fetches both the list and the narrative —
useful after the ticket got a transition or a new comment server-side.

Atlassian Document Format (Jira's rich-text JSON) is rendered as plain
text in v1; inline marks (bold, italic, links) are stripped and block
structure (paragraphs, bullet lists, code blocks) is preserved by
newlines.

### Status transition picker

`t` opens a centered modal listing the workflow transitions available
for the focused ticket — exactly what Jira's web UI shows in the
"Status" dropdown, just keyboard-driven. Numbered `1`-`9` for direct
selection, `↑↓` / `jk` to move, Enter to commit, Esc to cancel.

The list is per-issue: Jira's workflow engine is graph-based, so the
options depend on the current status (what edges leave this node) and
your project permissions. A terminal state with no outgoing edges
renders as `(no transitions available)` — that's normal, not a bug.

On success the modal closes, the active tab refreshes (so the new
status chip shows up in the table), and any cached ticket detail
gets dropped so a re-fetch picks up the moved-to state. On failure
(permission denied, validation required) the error surfaces inside
the modal and the picker stays open.

### Filter

`/` opens a 1-row filter editor above the table. Substring match —
case-insensitive — against both `KEY` and `SUMMARY`. The visible row
count updates live as you type; `Enter` commits the filter (it stays
applied while you scroll, switch tabs, etc.); `Esc` cancels and drops
the filter.

Selection stays consistent across filter changes: arrow-keys step
through the *visible* rows only, and the cursor never lands on a
filtered-out row. Tab title shows `(<visible>/<total>)` while a
filter is active.

### Inline-edit assignee / fixVersion

`a` / `f` open a centered picker (type-to-filter editor + scrollable
list) populated from the project's assignable users / unreleased
versions respectively. Enter commits, Esc cancels. With at least one
row in the selection set, the commit applies to every selected ticket;
otherwise it applies to the focused row.

### Inline comment posting

With the detail panel open, `c` drops a one-block editor at the bottom
of the panel. Multi-line via `Enter`. `Ctrl+S` posts via the Jira REST
API (`POST /issue/{key}/comment`); `Esc` discards.

The posted comment shows up in the detail panel as soon as the
post-comment promise resolves — no manual refresh needed.

### Watcher toggle

`w` toggles your watch on the focused ticket. Direction is derived
from the cached ticket detail (it includes `isWatching` alongside
the watcher count), so the first press fetches the detail if the
right-half panel hasn't already loaded it.

The detail panel header shows `★ watching (4 total)` or
`☆ 2 watcher(s)` — a glance at how many people are paying attention,
and whether you're one of them.

Unwatch requires Jira's `accountId`, which is fetched once per
session via `/rest/api/3/myself` and cached. If that call fails
(revoked token / scope), `w` toasts the error and falls back to
no-op rather than retrying every keypress.

### Per-tab column override

Each `[[tabs]]` entry can override the default column set via
`columns = [...]`. Default (when unset) is
`["key", "status", "assignee", "updated", "summary"]`. Valid values:

| value          | column            |
|----------------|-------------------|
| `key`          | TE-1234           |
| `status`       | "In Review" etc.  |
| `assignee`     | display name      |
| `reporter`     | display name      |
| `priority`     | "Highest" etc.    |
| `type`         | "Bug", "Task", …  |
| `updated`      | YYYY-MM-DD        |
| `fix_version`  | comma-joined list |
| `summary`      | ticket title      |

`summary` is the only column that fills remaining width — put it
last. Example, replacing assignee with priority on the "Mine" tab:

```toml
[[tabs]]
name = "Mine"
jql  = "reporter = currentUser() ORDER BY updated DESC"
columns = ["key", "priority", "status", "updated", "summary"]
```

## Two run modes

### Standalone

```sh
mnml-tracker-jira
```

Works in any terminal. No mnml required.

### Blit-host (hosted as an mnml pane)

From inside mnml:

```vim
:host.launch mnml-tracker-jira
```

mnml spawns it with `--blit <socket>` and renders the streamed cell
grid as a native `Pane::BlitHost`. See [Building integrations](https://mnml.sh/manual/integrations/building/)
for the protocol details.

## Status

**v0.2** (current main + tagged release):

- Standalone + blit-host modes
- Configurable JQL or auto-resolved release tabs (`current_release`, `next_release`, optional component filter)
- 1-9 / Tab / Enter navigation · `r` refresh · `Esc` cascade
- `d` right-half detail panel — header + description + last 10 comments, lazy-loaded + cached per issue
- `/` filter editor — substring on key + summary, live update, selection stays consistent
- `t` status transition picker (single + bulk)
- `a` / `f` assignee + fixVersion pickers (single + bulk)
- `c` inline comment editor (`Ctrl+S` to post)
- `w` watcher toggle with `★`/`☆` chip
- `Space` multi-selection across rows
- Per-tab column override
- `--check` for resolved-config + auth verification

## License

MIT.
