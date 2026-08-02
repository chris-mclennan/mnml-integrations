# mnml-aws-cloudwatch-logs

CloudWatch Logs live tail viewer for [mnml](https://mnml.sh) —
terminal TUI with tabbed log groups, per-line severity coloring,
filter patterns, and a one-key jump to the CloudWatch console.
Runs standalone in any terminal or as a hosted mnml pane. Shells
out to the `aws` CLI; no SDK dependency.

```
┌─ cloudwatch logs ────────────────────────────────────────────────┐
│ ▸1.lambda errors · tailing  2.api gateway · tailing  3.ecs       │
└──────────────────────────────────────────────────────────────────┘
┌─ lambda errors · /aws/lambda/my-function ────────────────────────┐
│ 2026-06-06T15:43:01.234Z START RequestId: abc-123                 │
│ 2026-06-06T15:43:01.456Z [ERROR] DynamoDB throttled: …            │
│ 2026-06-06T15:43:01.789Z END RequestId: abc-123                   │
│ …                                                                 │
└──────────────────────────────────────────────────────────────────┘
  1-9 tab · ↑↓/jk scroll · y yank line · o console · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-aws-cloudwatch-logs mnml-aws-cloudwatch-logs
mnml-aws-cloudwatch-logs --install
```

You'll also need the [AWS CLI](https://aws.amazon.com/cli/) on
your `$PATH` with credentials configured (`aws configure` or the
usual environment variables / SSO).

## Config

```toml
# Optional top-level region (defers to AWS CLI's resolution):
# region = "us-east-1"

[[tabs]]
name = "lambda errors"
log_group = "/aws/lambda/my-function"
# Optional: narrow to one stream
# log_stream = "2026/06/06/[$LATEST]abc123"
# Optional: filter pattern (substring or CW Logs syntax)
filter = "ERROR"

[[tabs]]
name = "api gateway"
log_group = "/aws/apigateway/my-api"

[[tabs]]
name = "ecs service"
log_group = "/ecs/my-service"
```

`log_group` is required. `log_stream`, `region`, and `filter` are
optional per-tab. Filter syntax:
<https://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/FilterAndPatternSyntax.html>

## Auth shape

There is none on this viewer's side. Every operation is
`aws logs tail --follow …` as a subprocess. The CLI's credential
chain authenticates. Same shape as
[`mnml-aws-codebuild`](https://github.com/chris-mclennan/mnml-aws-codebuild)
— if one works, the other will.

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Scroll up / down (down at bottom = live-tail) |
| `PgUp` / `PgDn` | Page up / down |
| `g` / `G` | Top / bottom |
| `o` | Open CloudWatch console for the active tab |
| `y` | Yank focused line to OS clipboard |
| `q` / `Esc` / `Ctrl+C` | Quit |

## Two run modes

### Standalone

```sh
mnml-aws-cloudwatch-logs
```

### Blit-host (hosted by mnml)

```vim
:host.launch mnml-aws-cloudwatch-logs
```

## Status

**v0.1 (this release)** — Tabbed log groups, live tail with
severity coloring, filter patterns, console open, line yank,
5K-line scrollback per tab.

Held back for v0.2+:
- Multi-stream selection within a tab (currently one at most)
- CloudWatch Logs Insights queries
- Saved searches
- Log-group picker overlay (config-only today)

## License

MIT.
