# mnml-aws-rds

A terminal browser for [AWS RDS](https://aws.amazon.com/rds/) — list every DB instance or Aurora cluster in a region, inspect engine / status / endpoint / storage detail, yank the endpoint for a `psql` invocation in one keystroke. Runs **standalone in any terminal** or as a **native mnml pane** via the [blit-host protocol](https://mnml.sh/manual/integrations/building/).

Sibling to the rest of the AWS family — [`mnml-aws-codebuild`](https://github.com/chris-mclennan/mnml-aws-codebuild), [`mnml-aws-cloudwatch-logs`](https://github.com/chris-mclennan/mnml-aws-cloudwatch-logs), [`mnml-aws-amplify`](https://github.com/chris-mclennan/mnml-aws-amplify), [`mnml-aws-lambda`](https://github.com/chris-mclennan/mnml-aws-lambda), [`mnml-aws-eventbridge`](https://github.com/chris-mclennan/mnml-aws-eventbridge). Same `aws` CLI auth chain — no SDK dep.

```
┌─ rds ─────────────────────────────────────────────────────────────────┐
│ ▸1.Instances (8)  2.Clusters (3)                                      │
└───────────────────────────────────────────────────────────────────────┘
┌─ db instances (8) ────────────┐ ┌─ detail ────────────────────────────┐
│ ▸ prod-postgres  postgres · ⬤│ │ Identifier   prod-postgres          │
│   prod-readonly  postgres · ⬤│ │ Engine       postgres 16.4          │
│   stage-postgres postgres · ⬤│ │ Class        db.r6g.xlarge          │
│   thumb-cache    mysql · ⬤   │ │ Status       available              │
│   …                           │ │ Endpoint     prod.cluster-xyz.…    │
│                               │ │              :5432                   │
│                               │ │ Storage      200 GB · gp3           │
│                               │ │ Multi-AZ     true                   │
│                               │ │ AZ           us-east-1a             │
│                               │ │ Public       false                  │
│                               │ │ Master user  admin                  │
└───────────────────────────────┘ └─────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · o console · y yank ARN · E yank endpoint · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-aws-rds mnml-aws-rds
mnml-aws-rds --install
```

You'll also need the [AWS CLI](https://aws.amazon.com/cli/) on your `$PATH` with credentials configured.

## Setup

1. **Verify the AWS CLI works.** `aws rds describe-db-instances` must succeed.
2. **Run once** to scaffold the config: `mnml-aws-rds`.
3. **Edit `~/.config/mnml-aws-rds.toml`** — add your tabs.
4. **Re-run**.

## Auth shape

Pure shell-out to the `aws` CLI — same chain as the other AWS siblings.

## Config

```toml
# Optional top-level region:
# region = "us-east-1"

refresh_interval_secs = 60

[[tabs]]
name = "Instances"
kind = "instances"

[[tabs]]
name = "Clusters"
kind = "clusters"
```

### Tab kinds

| `kind` | What it shows |
|---|---|
| `instances` (default) | Every RDS DB instance in the region (Postgres / MySQL / MariaDB / Oracle / SQL Server) |
| `clusters` | Every Aurora cluster (DB Cluster identifier) — Postgres or MySQL |

## Layout

- **Tab strip:** one tab per `[[tabs]]` entry, with per-tab count badge
- **Items table (left, 45%):** identifier + engine · status
- **Detail panel (right, 55%):** focused item's full detail
  - **Instance:** identifier, engine + version, instance class, status, endpoint, storage size/type, multi-AZ, AZ, public/private, master username, cluster (if part of one), created, ARN
  - **Cluster:** identifier, engine + version, mode (provisioned/serverless), status, writer/reader endpoints, database name, multi-AZ, master username, allocated storage, created, ARN
- **Status:** active count, key hints

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` / `o` | Open RDS console for the focused item |
| `y` | Yank focused item's ARN to clipboard |
| `E` | Yank focused item's endpoint (host:port) — pipe into `psql` / `mysql` / etc. |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## Two run modes

### Standalone

```sh
mnml-aws-rds
```

### Blit-host (hosted by mnml)

```vim
:host.launch mnml-aws-rds
```

## Wire it into mnml's left rail

`mnml-aws-rds` ships as a default chip in mnml's rail under **INTEGRATIONS**. Bound to `<leader>i R` in the whichkey leader menu (vim mode), or palette-runnable as `forge.open_rds`.

## Status

**v0.1** — list (paginated) DB instances + Aurora clusters, focused-item detail panel, console open, ARN yank, endpoint yank.

Held back for v0.2+:
- Snapshot list per instance/cluster (`describe-db-snapshots`)
- Tag display in detail panel
- Cross-sibling handoff: `mnml-aws-cloudwatch-logs --log-group /aws/rds/instance/<id>/postgresql` (Postgres) / `/aws/rds/instance/<id>/error` (MySQL)
- Failover button for Aurora clusters
- Parameter group + option group browsing

## Source

[github.com/chris-mclennan/mnml-aws-rds](https://github.com/chris-mclennan/mnml-aws-rds). MIT.
