# mnml-cdn-cloudflare

A terminal browser for [Cloudflare](https://www.cloudflare.com/) — list zones color-coded by status, browse DNS records per zone, list Worker scripts and Pages projects, and watch security/firewall events. The first **CDN** sibling in the mnml family, talking to the Cloudflare API v4 directly (no SDK dep). Sits next to the AWS / DB / observability siblings.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane (see [TODO](#not-yet-supported) below).

```
┌─ cloudflare ─────────────────────────────────────────────────────────────┐
│ ▸1.Zones (12)  2.Workers (4)  3.Pages (3)  4.example.com DNS (28)        │
└──────────────────────────────────────────────────────────────────────────┘
┌─ zones (12) ───────────────────┐ ┌─ detail ──────────────────────────────┐
│ ▸ example.com                  │ │ Name             example.com          │
│   marketing.example.com        │ │ ID               abc123…              │
│   docs.example.com             │ │ Status           active               │
│   api.example.com              │ │ Plan             Pro                  │
│   …                            │ │ Paused           false                │
│                                │ │ Dev mode         off                  │
│                                │ │ Modified         2026-06-07T…         │
│                                │ │                                       │
│                                │ │  Name servers                         │
│                                │ │  alice.ns.cloudflare.com              │
│                                │ │  bob.ns.cloudflare.com                │
└────────────────────────────────┘ └───────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · o dashboard · y ID · X purge · D dev-mode · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-cdn-cloudflare
mnml-cdn-cloudflare --install
```

## Setup

1. **Create an API token.** dash.cloudflare.com → top-right user menu → **My Profile** → **API Tokens** → **Create Token**.
   - Start from the **"Edit zone DNS"** template (covers zone reads + DNS read/write).
   - **Add Account-level scopes** required for the Workers / Pages tabs:
     - `Account` → `Workers Scripts` → **Read**
     - `Account` → `Pages` → **Read**
   - **Zone-level scopes** required for the cache-purge + dev-mode actions:
     - `Zone` → `Zone` → **Read**
     - `Zone` → `DNS` → **Read** (plus Write if you want v0.2 DNS edits to work)
     - `Zone` → `Cache Purge` → **Edit**
     - `Zone` → `Zone Settings` → **Edit** (covers development mode)
   - Optional but useful for `--check`: `User` → `User Details` → **Read** (lets `--check` print the token owner's email).
2. **Set env vars.**
   ```sh
   export CLOUDFLARE_API_TOKEN=...      # the token you just created
   export CLOUDFLARE_ACCOUNT_ID=...     # dash.cloudflare.com sidebar → Account ID
                                        # (required for the workers / pages tabs only)
   ```
3. **Run once** to scaffold the config:
   ```sh
   mnml-cdn-cloudflare
   ```
4. **Edit** `~/.config/mnml-cdn-cloudflare/config.toml` — add your tabs.
5. **Re-run.**

`mnml-cdn-cloudflare --check` prints the resolved config, which env vars are set, hits `GET /user/tokens/verify`, and surfaces the token id + status + owner email.

## Security

API tokens grant broad access to your CDN, DNS, Workers, and Pages. Treat them like passwords:

- Use a **scoped token** (not your Global API Key — that one's all-or-nothing).
- Scope the token to only the zones / accounts you need.
- Don't commit it to git; export it from a shell init that's outside any repo.
- Rotate periodically.

## Auth shape

Plain HTTP — every request carries `Authorization: Bearer <token>` and hits `https://api.cloudflare.com/client/v4/...`. No SDK dep.

Cloudflare wraps every response in:

```json
{"success": true | false, "errors": [{"code": N, "message": "..."}], "result": ...}
```

`mnml-cdn-cloudflare` unwraps `result` on success and surfaces the first `errors[].message` on failure. 403s get an extra "token missing required scope" hint since that's the most common config issue.

## Config

```toml
refresh_interval_secs = 60

[[tabs]]
name = "Zones"
kind = "zones"

# Per-zone DNS — set zone_id to enable.
# Find the zone_id with `mnml-cdn-cloudflare` running, focusing the
# Zones tab, and pressing `y` to yank it.
[[tabs]]
name = "example.com DNS"
kind = "dns"
zone_id = "abc123..."

[[tabs]]
name = "Workers"
kind = "workers"

[[tabs]]
name = "Pages"
kind = "pages"

# Per-zone security / firewall events.
[[tabs]]
name = "example.com WAF"
kind = "security_events"
zone_id = "abc123..."
```

### Tab kinds

| `kind` | What it shows | Required fields |
|---|---|---|
| `zones` | Every zone (status color: active=green, pending=yellow, suspended=red), with plan + paused chip | none |
| `dns` | DNS records for one zone — A/AAAA cyan, CNAME blue, MX yellow, TXT gray; proxied=orange chip | `zone_id` |
| `workers` | Worker scripts (name, last modified, created_on); detail shows usage model | none (uses `CLOUDFLARE_ACCOUNT_ID`) |
| `pages` | Pages projects (name, primary domain, production branch, last deploy status) | none (uses `CLOUDFLARE_ACCOUNT_ID`) |
| `security_events` | Recent firewall events for one zone (timestamp, client IP, action, source, rule) | `zone_id` |

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` / `o` | Open in the Cloudflare dashboard (zones / DNS / workers / pages / security events) |
| `y` | Yank — focused item's ID (zone ID, record ID, script name, project ID, ray ID) |
| `X` | **Purge cache** for the focused zone (zones tab only). Confirms with `[y/n]` before issuing `POST /zones/{id}/purge_cache {"purge_everything": true}`. |
| `D` | **Toggle development mode** for the focused zone (zones tab only). Issues `PATCH /zones/{id}/settings/development_mode`. |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## API endpoints used

| Tab / action | Endpoint |
|---|---|
| `--check` | `GET /user/tokens/verify`, `GET /user` |
| `zones` | `GET /zones?per_page=50` |
| zone detail (post-D) | `GET /zones/{id}` |
| `dns` | `GET /zones/{zone_id}/dns_records?per_page=200` |
| `workers` | `GET /accounts/{account_id}/workers/scripts` |
| worker routes (helper) | `GET /accounts/{account_id}/workers/scripts/{name}/routes` |
| `pages` | `GET /accounts/{account_id}/pages/projects` |
| `security_events` | `GET /zones/{zone_id}/security/events?limit=100` |
| `X` purge | `POST /zones/{id}/purge_cache` |
| `D` dev mode | `PATCH /zones/{id}/settings/development_mode` |

## Rate limits

Cloudflare's global REST limit is **1200 requests per 5-minute window** per user/token. `mnml-cdn-cloudflare` polls at the configured `refresh_interval_secs` (default 60s) per focused tab — well under the limit for normal use. A formal rate-limit counter with an 80% warning chip is on the v0.2 list.

## Pagination

v0.1 caps each list at **500 items** to keep the UI snappy. When the cap is hit, the tab badge shows `(N+)` so you know the list was truncated. Real cursor pagination (continuing past 500) is on the v0.2 list. Security events are capped server-side at 100 per request (no client-side cap).

## Run modes

### Standalone

```sh
mnml-cdn-cloudflare
```

### Blit-host (hosted by mnml)

Not yet — v0.1 is standalone-only. v0.2 will add the `--blit <socket>` mode so mnml can launch it as a native pane (the same shape the AWS family already supports).

## Wire it into mnml's left rail

`mnml-cdn-cloudflare` will ship as a default chip in mnml's rail under **INTEGRATIONS** once blit-host mode lands. For v0.1, the standalone binary is on `$PATH` after `cargo install` and the integration overlay picks it up.

## Not yet supported

Held back for v0.2+:

- **Blit-host pane mode** so mnml can host it as a native pane (the v0.1 priority follow-up).
- **DNS record editing** — v0.1 is read-only. POST/PATCH/DELETE on records is queued for v0.2.
- **Creating Workers** — `PUT /accounts/{id}/workers/scripts/{name}` etc.
- **Deploying Pages** — `POST /accounts/{id}/pages/projects/{name}/deployments`.
- **R2 / KV / D1** — separate top-level resources; each is worth a dedicated tab kind.
- **Page Rules editor** — list + create + edit page rules.
- **Firewall rules editor** — list + create + edit WAF custom rules.
- **Analytics graphs** — v0.1 surfaces security events; richer analytics (HTTP request volume, bandwidth, threat counts) would mean either GraphQL Analytics or text-only stats.
- **Cursor pagination past 500 items.**
- **Rate-limit counter** — v0.1 doesn't track the 1200/5min ceiling.

## Status

**v0.1** — zones / DNS / workers / pages / security_events tabs, color-coded by state, detail pane, dashboard open, ID yank, cache purge (with confirmation), dev-mode toggle. Standalone only.

## Source

[github.com/chris-mclennan/mnml-cdn-cloudflare](https://github.com/chris-mclennan/mnml-cdn-cloudflare). MIT.
