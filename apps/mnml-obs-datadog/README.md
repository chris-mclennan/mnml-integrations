# mnml-obs-datadog

A terminal browser for [Datadog](https://www.datadoghq.com/) — list monitors color-coded by alert state, browse dashboards, live-tail logs against a custom query, and watch open incidents. The first **observability** sibling in the mnml family, and the first sibling to talk to a JSON HTTP API directly (everything before it shelled out to a vendor CLI). Sits next to the AWS / DB / forge / tracker siblings and hands monitors off to `mnml-aws-cloudwatch-logs` when a Datadog monitor query references an AWS log group.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane (see [TODO](#not-yet-supported) below).

```
┌─ datadog ─────────────────────────────────────────────────────────────┐
│ ▸1.Monitors (37)  2.Dashboards (84)  3.API errors (12)  4.Incidents (2)│
└───────────────────────────────────────────────────────────────────────┘
┌─ monitors (37) ───────────────┐ ┌─ detail ────────────────────────────┐
│ ▸ api 5xx rate                │ │ Name             api 5xx rate       │
│   db connection saturation    │ │ Type             metric alert       │
│   queue backlog               │ │ State            Alert              │
│   high cpu — i-aabbccdd       │ │ ID               1234567            │
│   nightly batch ran           │ │ Tags             service:api        │
│   …                           │ │                                     │
│                               │ │  Query                              │
│                               │ │  avg(last_5m):sum:trace.http…       │
│                               │ │                                     │
└───────────────────────────────┘ └─────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · o console · y URL · L jump · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-obs-datadog
mnml-obs-datadog --install
```

## Setup

1. **Auth (env vars).** Datadog uses two keys: an API key (for the org) + an application key (for the user). Both required.
   ```sh
   export DD_API_KEY=...      # Org Settings → API Keys
   export DD_APP_KEY=...      # Org Settings → Application Keys
   export DD_SITE=datadoghq.com   # default; override for EU / US3 / US5 / AP1 / Gov
   ```
2. **Run once** to scaffold the config:
   ```sh
   mnml-obs-datadog
   ```
3. **Edit** `~/.config/mnml-obs-datadog/config.toml` — add your tabs.
4. **Re-run.**

`mnml-obs-datadog --check` prints the resolved config + which env vars are set + the API base URL.

## Auth shape

Plain HTTP — every request carries `DD-API-KEY` + `DD-APPLICATION-KEY` headers and hits `https://api.{DD_SITE}/api/v1/...` or `/api/v2/...`. No SDK dep.

## Config

```toml
refresh_interval_secs = 60

[[tabs]]
name = "Monitors"
kind = "monitors"

# Scope monitors by tag —
[[tabs]]
name = "api alerts"
kind = "monitors"
query = "tag:service:api"

[[tabs]]
name = "Dashboards"
kind = "dashboards"

# Title-prefix filter on dashboards —
[[tabs]]
name = "API dashboards"
kind = "dashboards"
query = "API"

# Live-tail logs — query uses Datadog log search syntax,
# polled every `tail_interval_secs` (defaults to 5s) when focused:
[[tabs]]
name = "API errors"
kind = "logs"
query = "service:api status:error"
from = "now-15m"
tail_interval_secs = 5

[[tabs]]
name = "Incidents"
kind = "incidents"
```

### Tab kinds

| `kind` | What it shows | Required fields |
|---|---|---|
| `monitors` | Every monitor, sorted Alert → Warn → No Data → OK; `query` is a tag scope (e.g. `tag:service:api`) | none |
| `dashboards` | Every dashboard with title, author, id; `query` is a title-prefix filter | none |
| `logs` | Live-tail of recent logs matching `query` (Datadog log search syntax) over the `from` time window | `query` |
| `incidents` | Open (state=active) incidents | none |

## Layout

- **Tab strip:** one tab per `[[tabs]]` entry, with per-tab count badge. `(N+)` means the API returned more than the v0.1 cap (500 items) and the list was truncated.
- **Items table (left, 45%):**
  - **Monitors:** `<name>  <state> · <kind>`. Color cues — `Alert` red, `Warn` / `No Data` yellow, `OK` green, otherwise gray.
  - **Dashboards:** `<title>  <author>`.
  - **Logs:** `<service>  <HH:MM:SS> [<status>] <first line of message>`. `error` / `critical` red, `warn` yellow.
  - **Incidents:** `<title>  <severity> · <state>`. SEV-1 / SEV-2 red, SEV-3 yellow.
- **Detail panel (right, 55%):** focused item's full detail.
  - **Monitor:** name, type, state, ID, last-modified, tags, query body, alert message.
  - **Dashboard:** title, ID, author, layout type, last-modified, source path.
  - **Log:** timestamp, service, status, host, event ID, message body (first 20 lines).
  - **Incident:** title, public ID, state, severity, created timestamp, UUID.

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` / `o` | Open in Datadog web UI (monitor / dashboard / incident page; logs tab opens the Logs Explorer pre-scoped to the tab's query) |
| `y` | Yank — web URL for monitor / dashboard / incident; log message body for log events |
| `L` | Cross-sibling jump — on a monitor whose query references an AWS log group (`aws_log_group:` tag or `/aws/...` path), launch `mnml-aws-cloudwatch-logs --log-group <group>`. Best-effort detection. |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## API endpoints used

| Tab | Endpoint |
|---|---|
| `monitors` | `GET /api/v1/monitor` (optional `monitor_tags=...` query) |
| `dashboards` | `GET /api/v1/dashboard` |
| `logs` | `POST /api/v2/logs/events/search` (filter / page / sort) |
| `incidents` | `GET /api/v2/incidents?filter[state]=active` |

## Pagination

v0.1 caps each list at **500 items** to keep the UI snappy. When the cap is hit, the tab badge shows `(N+)` so you know the list was truncated. Real cursor pagination (continuing past 500) is on the v0.2 list.

## Run modes

### Standalone

```sh
mnml-obs-datadog
```

### Blit-host (hosted by mnml)

Not yet — v0.1 is standalone-only. v0.2 will add the `--blit <socket>` mode so mnml can launch it as a native pane (the same shape the AWS family already supports). Until then, run it in a sibling tmnl tab.

## Wire it into mnml's left rail

`mnml-obs-datadog` will ship as a default chip in mnml's rail under **INTEGRATIONS** once blit-host mode lands. For v0.1, the standalone binary is on `$PATH` after `cargo install` and the integration overlay picks it up.

## Not yet supported

Held back for v0.2+:

- **Blit-host pane mode** so mnml can host it as a native pane (the v0.1 priority follow-up).
- **Cursor pagination** — v0.1 caps lists at 500 and surfaces a `(N+)` hint.
- **Traces** — the v2 Trace Search API is its own shape and worth a dedicated tab kind.
- **Monitor mute / unmute** — `POST /monitor/{id}/mute` + `DELETE /monitor/{id}/mute`.
- **Dashboard edit** — opens the editor in the browser for now; in-TUI editing is out of scope.
- **Incident deep-dive** — timeline, tasks, attachments, integrations.
- **Live-tail cursor** — v0.1 re-issues the full query every `tail_interval_secs`; a proper cursor would only fetch new events.
- **Per-monitor downtime scheduling.**

## Status

**v0.1** — monitors / dashboards / logs (live-tail) / incidents tabs, color-coded by state, detail pane, console open, URL yank, log message yank, cross-sibling handoff to `mnml-aws-cloudwatch-logs` for AWS-log-group monitors. Standalone only.

## Source

[github.com/chris-mclennan/mnml-obs-datadog](https://github.com/chris-mclennan/mnml-obs-datadog). MIT.
