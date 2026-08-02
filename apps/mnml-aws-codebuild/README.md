# mnml-aws-codebuild

AWS CodeBuild + CloudWatch viewer for [mnml](https://mnml.sh) —
terminal TUI for recent builds and live log tail. Runs standalone
in any terminal or as a hosted mnml pane. Shells out to the `aws`
CLI; no AWS SDK dependency.

```
┌─ aws ────────────────────────────────────────────────────────────┐
│ ▸1.api builds (30)  2.api logs · tailing                         │
└──────────────────────────────────────────────────────────────────┘
┌─ api builds ─────────────────────────────────────────────────────┐
│ #       │ STATUS       │ STARTED          │ DUR │ INITIATOR      │
│ #38788  │ ✓ succeeded  │ 2026-06-05 14:01 │ 96s │ codepipeline…  │
│ #38787  │ ✗ failed     │ 2026-06-05 13:42 │ 29s │ chris@mnml     │
│ …                                                                 │
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-aws-codebuild mnml-aws-codebuild
mnml-aws-codebuild --install
```

You'll also need the [AWS CLI](https://aws.amazon.com/cli/)
configured with credentials that can read CodeBuild and CloudWatch
Logs.

## Setup

1. **Make sure the `aws` CLI is on PATH and authenticated.** This
   binary defers entirely to the AWS CLI's credential chain — set
   `AWS_PROFILE` / `AWS_REGION` / run `aws configure` the same way
   you would for any other AWS tool.

2. **Run once** to scaffold the config template:

   ```sh
   mnml-aws-codebuild
   ```

   Writes `~/.config/mnml-aws-codebuild.toml`. Edit the `[[tabs]]`
   list — pick a CodeBuild project name and (optionally) a
   CloudWatch log group.

3. **Re-run** — the TUI launches with your configured tabs.

4. **Verify** the resolved config without launching the TUI:

   ```sh
   mnml-aws-codebuild --check
   ```

## Config shape

```toml
# region is optional — defers to the `aws` CLI's resolution
# region = "us-east-1"
refresh_interval_secs = 60

[[tabs]]
name    = "api builds"
project = "my-app"            # CodeBuild project name

[[tabs]]
name      = "api logs"
kind      = "logs"            # `aws logs tail --follow`
log_group = "/aws/codebuild/my-app"
# log_stream = "abc123"       # optional — narrows to one stream
```

## Tab kinds

| kind | Required fields | What it shows |
|---|---|---|
| `builds` (default) | `project` | Most-recent CodeBuild runs (#, status, started, duration, initiator, source ref) |
| `logs` | `log_group` | Live `aws logs tail --follow` with per-line severity coloring (ERROR/WARN/INFO/DEBUG) |

Logs tabs spawn `aws logs tail --follow` on first activation and
keep the child running until the tab is closed (or the binary
exits — the child is killed on Drop).

## Keys

| key            | action                              |
| -------------- | ----------------------------------- |
| `1`–`9`        | switch to tab by index              |
| `Tab` / `S-Tab`| next / previous tab                 |
| `↑` / `k`      | move selection up / scroll logs up  |
| `↓` / `j`      | move selection down / scroll down   |
| `PgUp` / `PgDn`| page up / down                      |
| `g` / `G`      | home / end                          |
| `Enter` / `o`  | open focused build in browser       |
| `r`            | refresh active tab                  |
| `q` / `Esc`    | quit                                |

## Use it as an mnml pane

`mnml-aws-codebuild` speaks the `tmnl-protocol` blit-host shape
when launched with `--blit <socket>` — so mnml can host it inside
a regular pane:

```
:host.launch mnml-aws-codebuild
```

## Status

v0.1 — Builds + Logs tabs. No build-detail panel yet, no
`fetch artifact → Tests pane` cross-nav. Both queued for v0.2.
