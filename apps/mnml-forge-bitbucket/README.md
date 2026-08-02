# mnml-forge-bitbucket

Bitbucket Cloud pull-request viewer for [mnml](https://mnml.sh) —
terminal TUI with configurable tabs (per-repo, "PRs I opened",
"PRs I'm reviewing"). Runs standalone in any terminal or as a
hosted mnml pane. Member of the `mnml-tickets-*` integration class
alongside [jira](https://github.com/chris-mclennan/mnml-tickets-jira)
and [github](https://github.com/chris-mclennan/mnml-tickets-github).

```
┌─ bitbucket PRs ──────────────────────────────────────────────────┐
│ ▸1.Mine (3)  2.Reviewing (7)  3.example-api PRs (12)              │
└──────────────────────────────────────────────────────────────────┘
┌─ Mine ───────────────────────────────────────────────────────────┐
│ REPO              │ PR     │ STATE │ AUTHOR  │ BRANCH → DEST     │
│ acme/api   │ #1234  │ OPEN  │ Chris   │ chris/fix → main  │
│ acme/web   │ #821   │ OPEN  │ Chris   │ chris/redesign…   │
│ …                                                                 │
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-forge-bitbucket mnml-forge-bitbucket
mnml-forge-bitbucket --install
```

Homebrew tap + binary releases will follow once the binary stabilises.

## Setup

1. **Create a Bitbucket app password** at
   <https://bitbucket.org/account/settings/app-passwords/>.

   Minimum scopes: **Pull requests: Read**. Add **Account: Read** if
   you want `mode = "mine"` / `mode = "reviewing"` tabs (those need
   `/2.0/user` to resolve your account_id).

2. **Point the tool at your app password** — one of:

   - **Env var** (preferred if your shell already exports
     Bitbucket credentials):

     ```sh
     export BITBUCKET_APP_PASSWORD="ATATT-…"           # bare app password
     # OR
     export BITBUCKET_PERSONAL_TOKEN="me@x.com:ATATT-…"  # email:token combined form
     ```

     `BITBUCKET_APP_PASSWORD` wins over `BITBUCKET_PERSONAL_TOKEN`;
     the latter accepts either the bare token or the
     `email:token` combined form (only the after-colon portion
     is used).

   - **On-disk file** (chmod 600):

     ```sh
     mkdir -p ~/.config/mnml-forge-bitbucket
     pbpaste > ~/.config/mnml-forge-bitbucket/token   # or paste it in $EDITOR
     chmod 600 ~/.config/mnml-forge-bitbucket/token
     ```

   Resolution order at startup: `BITBUCKET_APP_PASSWORD` →
   `BITBUCKET_PERSONAL_TOKEN` → `~/.config/mnml-forge-bitbucket/token`.

3. **Run once** to scaffold the config template:

   ```sh
   mnml-forge-bitbucket
   ```

   Writes `~/.config/mnml-forge-bitbucket.toml`. Edit `email`,
   `workspace`, and the `[[tabs]]` list.

4. **Re-run** — the TUI launches with your configured tabs.

5. **Verify** the resolved config + auth state:

   ```sh
   mnml-forge-bitbucket --check
   ```

   Hits `/2.0/user` to confirm the app password works. Reports
   the token source (`env: BITBUCKET_APP_PASSWORD` /
   `env: BITBUCKET_PERSONAL_TOKEN` / the file path) so you can
   confirm which credential path is authoritative.

## Tab kinds

Each `[[tabs]]` entry is one tab. The `kind` field (defaults to `pull_requests`) decides what the tab shows:

| `kind` | What it shows | Required fields |
|---|---|---|
| `pull_requests` (default) | PR list, with state filter + optional mine/reviewing modes | one of `repo` / `mode` / `q` |
| `pipelines` | Recent builds for a repo, newest-first | `repo` |
| `branches` | Branches in a repo, sorted by latest commit | `repo` |

PR-specific fields (`state`, `mode`, `q`) are ignored on `pipelines` and `branches` tabs.

```toml
[[tabs]]
name = "Reviewing"
mode = "reviewing"          # kind defaults to pull_requests

[[tabs]]
name = "example-api pipelines"
kind = "pipelines"
repo = "example-api"

[[tabs]]
name = "example-api branches"
kind = "branches"
repo = "example-api"
```

### Pull-request tab shapes

Three shapes for `kind = "pull_requests"`:

### Per-repo

```toml
[[tabs]]
name  = "example-api PRs"
repo  = "example-api"
state = "OPEN"             # OPEN / MERGED / DECLINED / SUPERSEDED
```

Uses the default workspace from the top of the config. Override
per-tab with `workspace = "otherws"` if needed.

### `mode = "mine"` — PRs you opened

```toml
[[tabs]]
name = "Mine"
mode = "mine"
```

Bitbucket Cloud has no workspace-scoped PR endpoint that
accepts BBQL, so `mode = "mine"` enumerates every repo you have
access to in the workspace and fans out per-repo BBQL queries
(`author.account_id = "<your-id>"`) with a concurrency cap of 8.
Results are merged + sorted by `updated_on` descending. On a
100-repo workspace this typically completes in a few seconds;
per-repo errors (e.g. a 403 on an archived repo) are dropped
silently rather than blanking the tab.

Requires **Account: Read** on the app password.

### `mode = "reviewing"` — PRs you're a reviewer on

```toml
[[tabs]]
name = "Reviewing"
mode = "reviewing"
```

Same enumeration shape as `mine`, but the per-repo BBQL swaps to
`reviewers.account_id = "<your-id>"`. Requires **Account: Read**.

### Custom BBQL

For finer-grained control you can supply a raw Bitbucket Query
Language string via `q`. Either as the only filter (no `mode`,
no `repo`) or layered on top of an auto-mode tab:

```toml
[[tabs]]
name  = "Stale PRs"
repo  = "example-api"
state = "OPEN"
q     = "updated_on <= 2026-05-01T00:00:00+00:00"
```

```toml
[[tabs]]
name = "My recent merges"
mode = "mine"
state = "MERGED"
q    = "updated_on >= 2026-05-01"
```

BBQL reference:
<https://developer.atlassian.com/cloud/bitbucket/rest/intro/#filtering-and-sorting-results>

## Keys

| Chord                | Action                                            |
|----------------------|---------------------------------------------------|
| `1`-`9`              | Switch to that tab                                |
| `Tab` / `BackTab`    | Cycle tabs forward / back                         |
| `↑` / `k`, `↓` / `j` | Move selection                                    |
| `PgUp` / `PgDn`      | Jump 10 rows                                      |
| `g` / `G`            | Top / bottom                                      |
| `Enter` / `o`        | Open focused PR in your browser                   |
| `d`                  | Toggle right-half PR detail panel                 |
| `Ctrl+U` / `Ctrl+D`  | Scroll detail panel up / down (when open)         |
| `a`                  | Toggle your approval on the focused PR (detail panel must be open) |
| `r`                  | Refresh active tab (+ detail if open)             |
| `q` / `Esc` / `Ctrl+C` | Quit                                            |

Auto-refresh runs every `refresh_interval_secs` seconds (default `60`,
set to `0` to disable).

### Detail panel

`d` opens a right-half panel for the focused PR: header (state ·
branches · author · updated · approval chip), then title, description,
then up to the last 20 comments (most-recent first). Detail content is
lazy-loaded on first focus and cached per `(workspace, repo, id)` —
arrow-keying through a long list only fetches once per PR.

`r` while the detail panel is open invalidates the cached detail for
the focused PR and re-fetches both the list and the detail — useful
after a new comment landed server-side.

The approval chip shows either `✓ you approved · N total` or
`○ not approved · N total`. `N` is the count of approving participants
on the PR (including you).

### Approve / unapprove

`a` (with the detail panel open) toggles your approval. The viewer
reads the current state from the cached participant record and POSTs
or DELETEs `/pullrequests/{id}/approve` accordingly, then drops the
cache so a re-fetch picks up the new state.

Requires **Account: Read** on the app password — otherwise the viewer
can't resolve your `account_id` and the toggle is a no-op with an
explanatory toast.

## Two run modes

### Standalone

```sh
mnml-forge-bitbucket
```

Works in any terminal. No mnml required.

### Blit-host (hosted as an mnml pane)

```vim
:host.launch mnml-forge-bitbucket
```

mnml spawns it with `--blit <socket>` and renders the streamed cell
grid as a native `Pane::BlitHost`. See
[Building integrations](https://mnml.sh/manual/integrations/building/)
for the protocol details.

## Wire it into mnml's left rail

```toml
[[ui.integration_icon]]
id       = "bitbucket"
glyph    = "\U000F0093"            # nf-md-bitbucket
fallback = "B"
command  = ":host.launch mnml-forge-bitbucket"
color    = "blue"
tooltip  = "Open Bitbucket PRs"
```

Setting `[[ui.integration_icon]]` **replaces** mnml's built-in
defaults, so copy them in first if you want to extend rather than
replace.

## Limitations

- **PRs only.** Bitbucket Cloud's issue tracker isn't covered — most
  teams use Jira for issues. Add it if anyone wants it.
- **First page only.** The Bitbucket API caps `pagelen` at 50 and the
  viewer doesn't paginate yet. For tabs with >50 open PRs, the oldest
  are truncated.
- **First-comment-page only on detail.** Same `pagelen` cap on
  `/pullrequests/{id}/comments`. v0.3.
- **Merge / decline aren't wired.** v0.3 — both are behind a
  confirmation modal once added.

## Status

**v0.3** (current):

- Standalone + blit-host modes
- **Three tab kinds**: `pull_requests` (default), `pipelines`, `branches`
- PR tabs:
  - Per-repo with state filter (OPEN / MERGED / DECLINED / SUPERSEDED)
  - `mode = "mine"` / `mode = "reviewing"` auto-tabs (workspace-spanning BBQL)
  - Custom `q` BBQL for layered filters
- Pipeline tabs: build number / state / branch / commit / trigger / duration, newest-first
- Branch tabs: name / sha / latest commit date / author / first-line commit message
- 1-9 / Tab / Enter navigation · `r` refresh · `Enter`/`o` open in browser (kind-aware URL)
- Auto-refresh on `refresh_interval_secs`
- `d` right-half PR detail panel — header + description + last 20 comments, lazy-loaded + cached
- `Ctrl+U` / `Ctrl+D` scroll the detail panel
- `a` approve / unapprove toggle with `✓ you approved · N total` chip
- `--check` for resolved-config + whoami verification

## License

MIT.
