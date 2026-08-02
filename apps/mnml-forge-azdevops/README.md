# mnml-forge-azdevops

Azure DevOps viewer for [mnml](https://mnml.sh) — terminal TUI
with configurable tabs (per-repo PRs, "PRs I created", "PRs I'm
reviewing", project builds). Runs standalone in any terminal or
as a hosted mnml pane. Member of the `mnml-forge-*` integration
class alongside
[bitbucket](https://github.com/chris-mclennan/mnml-forge-bitbucket)
and [github](https://github.com/chris-mclennan/mnml-forge-github).

```
┌─ azure devops ───────────────────────────────────────────────────┐
│ ▸1.Mine (4)  2.Reviewing (6)  3.api PRs (12)  4.Builds (50)      │
└──────────────────────────────────────────────────────────────────┘
┌─ Mine ───────────────────────────────────────────────────────────┐
│ ID    │ STATUS  │ REPO     │ SRC → DEST          │ TITLE         │
│ #421  │ active  │ api      │ feat/x → main       │ Add /v2 …     │
│ #418  │ active  │ web      │ chore/deps → main   │ Bump axios …  │
│ …                                                                 │
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-forge-azdevops mnml-forge-azdevops
mnml-forge-azdevops --install
```

Homebrew tap + binary releases will follow once the binary stabilises.

## Setup

1. **Create an Azure DevOps PAT** at
   `https://dev.azure.com/<org>/_usersSettings/tokens`.

   Minimum scopes: **Code (Read)** for PR tabs and **Build (Read)**
   for build tabs. Add **User Profile (Read)** if you want
   `mode = "mine"` / `mode = "reviewing"` tabs (those resolve the
   current user's GUID via `/_apis/connectionData`).

2. **Save the PAT** to `~/.config/mnml-forge-azdevops/token` with
   `chmod 600`:

   ```sh
   mkdir -p ~/.config/mnml-forge-azdevops
   pbpaste > ~/.config/mnml-forge-azdevops/token   # or paste it in $EDITOR
   chmod 600 ~/.config/mnml-forge-azdevops/token
   ```

3. **Run once** to scaffold the config template:

   ```sh
   mnml-forge-azdevops
   ```

   Writes `~/.config/mnml-forge-azdevops.toml`. Edit `org`,
   `project`, and the `[[tabs]]` list.

4. **Re-run** — the TUI launches with your configured tabs.

5. **Verify** the resolved config + auth state:

   ```sh
   mnml-forge-azdevops --check
   ```

## Config shape

```toml
org     = "acme"      # required: <org> in dev.azure.com/<org>/
project = "Example"         # optional default; tabs can override

refresh_interval_secs = 60

[[tabs]]
name = "Mine"
mode = "mine"               # needs User Profile (Read) scope

[[tabs]]
name = "Reviewing"
mode = "reviewing"

[[tabs]]
name = "api PRs"
repo  = "api"
state = "active"            # active | completed | abandoned | all

[[tabs]]
name = "Builds"
kind  = "builds"            # project-scoped; repo/branch/definition optional
```

## Keys

| key            | action                              |
| -------------- | ----------------------------------- |
| `1`–`9`        | switch to tab by index              |
| `Tab` / `S-Tab`| next / previous tab                 |
| `↑` / `k`      | move selection up                   |
| `↓` / `j`      | move selection down                 |
| `PgUp` / `PgDn`| page up / down                      |
| `g` / `G`      | home / end                          |
| `Enter` / `o`  | open focused row in browser         |
| `r`            | refresh active tab                  |
| `q` / `Esc`    | quit                                |

## Use it as an mnml pane

`mnml-forge-azdevops` speaks the `tmnl-protocol` blit-host shape
when launched with `--blit <socket>` — so mnml can host it inside
a regular pane:

```
:host.launch mnml-forge-azdevops
```

(or seed the bufferline launcher chip via `[[ui.launcher_icon]]`).

## Status

v0.1 — Pull-requests + Builds tabs, project-spanning `mine` /
`reviewing` PR modes. No detail panel yet (`d` on a PR row opens
the page in the browser; a right-half PR detail panel is queued
for v0.2 alongside the comments / diff endpoints).
