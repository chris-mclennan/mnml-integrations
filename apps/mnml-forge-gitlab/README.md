# mnml-forge-gitlab

GitLab viewer for [mnml](https://mnml.sh) — terminal TUI with
configurable tabs (per-project MRs, per-project pipelines, "MRs I
opened", "MRs I'm reviewing"). Runs standalone in any terminal or
as a hosted mnml pane. Member of the `mnml-forge-*` integration
class alongside
[bitbucket](https://github.com/chris-mclennan/mnml-forge-bitbucket),
[github](https://github.com/chris-mclennan/mnml-forge-github),
and [azdevops](https://github.com/chris-mclennan/mnml-forge-azdevops).

```
┌─ gitlab ─────────────────────────────────────────────────────────┐
│ ▸1.Mine (5)  2.Reviewing (8)  3.api MRs (12)  4.api CI (30)      │
└──────────────────────────────────────────────────────────────────┘
┌─ Mine ───────────────────────────────────────────────────────────┐
│ !    │ STATE  │ PROJECT     │ SRC → DEST          │ TITLE        │
│ !421 │ opened │ org/api     │ feat/x → main       │ Add /v2 …    │
│ !418 │ opened │ org/web     │ chore/deps → main   │ Bump axios … │
│ …                                                                 │
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-forge-gitlab mnml-forge-gitlab
mnml-forge-gitlab --install
```

Homebrew tap + binary releases will follow once the binary
stabilises.

## Setup

1. **Create a GitLab personal access token** at
   `https://gitlab.com/-/user_settings/personal_access_tokens`
   (for self-hosted: `https://<your-gitlab>/-/user_settings/personal_access_tokens`).

   Minimum scope: **read_api**.

2. **Save the PAT** to `~/.config/mnml-forge-gitlab/token` with
   `chmod 600`:

   ```sh
   mkdir -p ~/.config/mnml-forge-gitlab
   pbpaste > ~/.config/mnml-forge-gitlab/token   # or paste in $EDITOR
   chmod 600 ~/.config/mnml-forge-gitlab/token
   ```

3. **Run once** to scaffold the config template:

   ```sh
   mnml-forge-gitlab
   ```

   Writes `~/.config/mnml-forge-gitlab.toml`. Edit `base_url` (only
   needed for self-hosted) and the `[[tabs]]` list.

4. **Re-run** — the TUI launches with your configured tabs.

5. **Verify** the resolved config + auth state:

   ```sh
   mnml-forge-gitlab --check
   ```

## Config shape

```toml
# base_url defaults to gitlab.com. Override for self-hosted:
# base_url = "https://gitlab.mycorp.com/api/v4"

refresh_interval_secs = 60

[[tabs]]
name = "Mine"
mode = "mine"                   # needs read_api scope on the PAT

[[tabs]]
name = "Reviewing"
mode = "reviewing"

[[tabs]]
name = "api MRs"
project = "your-group/api"
state   = "opened"              # opened | closed | merged | all

[[tabs]]
name    = "api pipelines"
kind    = "pipelines"
project = "your-group/api"
# ref_name = "main"              # optional branch filter
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

`mnml-forge-gitlab` speaks the `tmnl-protocol` blit-host shape when
launched with `--blit <socket>` — so mnml can host it inside a
regular pane:

```
:host.launch mnml-forge-gitlab
```

(or seed the bufferline launcher chip via `[[ui.launcher_icon]]`).

## Status

v0.1 — Merge-requests + Pipelines tabs, instance-spanning `mine` /
`reviewing` MR modes. No detail panel yet (`Enter` on an MR opens
the page in the browser; a right-half MR detail panel with notes
/ diff is queued for v0.2).
