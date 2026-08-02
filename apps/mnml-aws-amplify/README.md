# mnml-aws-amplify

AWS Amplify viewer for [mnml](https://mnml.sh) — terminal TUI for
Amplify apps, branches, and deploy jobs. Runs standalone in any
terminal or as a hosted mnml pane. Shells out to the `aws` CLI;
no SDK dependency.

```
┌─ amplify ────────────────────────────────────────────────────────┐
│ ▸1.All apps (12)  2.Frontend (4 br)  3.Marketing (2 br)          │
└──────────────────────────────────────────────────────────────────┘
┌─ Frontend ───────────────────────────────────────────────────────┐
│ ┌─ branches ──────────┐ ┌─ recent jobs ─────────────────────────┐│
│ │ ▸ main  PRODUCTION  │ │ #421 SUCCEED     a8f3c1d2 feat: …     ││
│ │   beta  BETA        │ │ #420 SUCCEED     b4e2c19a fix: …      ││
│ │   dev   DEVELOPMENT │ │ #419 FAILED      c9a1b3f5 chore: …    ││
│ │                     │ │ …                                     ││
│ └─────────────────────┘ └───────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter/o console · y yank · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-aws-amplify mnml-aws-amplify
mnml-aws-amplify --install
```

You'll also need the [AWS CLI](https://aws.amazon.com/cli/) on
your `$PATH` with credentials.

## Setup

1. Verify the AWS CLI works: `aws amplify list-apps` must succeed.
2. Run once to scaffold the config: `mnml-aws-amplify`.
3. Edit `~/.config/mnml-aws-amplify.toml`.
4. Re-run.

## Config

```toml
# Optional top-level region:
# region = "us-east-1"

refresh_interval_secs = 60

[[tabs]]
name = "All apps"
kind = "apps"

[[tabs]]
name = "Frontend"
kind = "app"
app_id = "d2abc123def456"   # from Amplify console URL or `aws amplify list-apps`
```

Two tab kinds:
- `apps` — list every Amplify app in the region (no required fields)
- `app` — drill into one specific app's branches + deploy jobs. Requires `app_id`.

The Amplify console URL has the app id in it: `https://us-east-1.console.aws.amazon.com/amplify/apps/<app_id>`.

## Auth shape

There is none on this viewer's side. Every operation is
`aws amplify list-…` as a subprocess. The CLI's credential chain
authenticates. Same shape as
[`mnml-aws-codebuild`](https://github.com/chris-mclennan/mnml-aws-codebuild)
and [`mnml-aws-cloudwatch-logs`](https://github.com/chris-mclennan/mnml-aws-cloudwatch-logs).

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `Enter` / `o` | Open focused row's console URL in browser |
| `y` | Yank focused row's console URL to OS clipboard |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## Status

**v0.1 (this release)** — Apps list, App detail (branches +
recent jobs split view), console open, URL yank. Standalone TUI
+ `--blit` host-pane mode.

Held back for v0.2+:
- Triggering a deploy from the terminal (start-job)
- Per-job log tail (deploy build logs)
- Pull request previews list (Amplify's PR previews are a
  separate API)
- Webhooks list

## License

MIT.
