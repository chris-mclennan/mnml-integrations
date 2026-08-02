# mnml-msg-mandrill

A terminal browser for [Mandrill](https://mandrillapp.com/) (Mailchimp Transactional) — list recent transactional sends color-coded by delivery state, browse templates by publish state, inspect tag stats with bounce-rate cues, and audit webhooks. The first **messaging** sibling in the mnml family.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane (see [Not yet supported](#not-yet-supported) below).

```
┌─ mandrill ────────────────────────────────────────────────────────────┐
│ ▸1.messages (87)  2.templates (12)  3.tags (24)  4.webhooks (3)        │
└───────────────────────────────────────────────────────────────────────┘
┌─ messages (87) ───────────────┐ ┌─ detail ────────────────────────────┐
│ ▸ Welcome aboard           │ │ Subject          Welcome aboard  │
│   Password reset              │ │ ID               abc123def456       │
│   Order #4421 confirmed       │ │ State            delivered          │
│   You have 3 new reviews      │ │ To               user@example.com   │
│   …                           │ │ From             noreply@example.com │
│                               │ │ Opens            2                  │
│                               │ │ Clicks           1                  │
│                               │ │ Tags             welcome, onboarding│
└───────────────────────────────┘ └─────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · o web · y ID · L jump · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-msg-mandrill
mnml-msg-mandrill --install
```

## Setup

1. **Auth (env var).** Mandrill uses a single API key per app — sent in the request body, not as a header.
   ```sh
   export MANDRILL_API_KEY=...   # Mandrill → Settings → SMTP & API Info → API Keys
   ```
   Either a **full-access** or a **read-only** key works for v0.1 — every endpoint we hit is read-only.
2. **Run once** to scaffold the config:
   ```sh
   mnml-msg-mandrill
   ```
3. **Edit** `~/.config/mnml-msg-mandrill/config.toml` if you want — the 4-tab default works out of the box.
4. **Re-run.**

`mnml-msg-mandrill --check` prints the resolved config + which env vars are set, hits `POST /users/ping.json`, and exits.

## Auth shape

Plain HTTP — every request is a `POST` to `https://mandrillapp.com/api/1.0/<resource>/<verb>.json`, and the request body always carries `"key": "<MANDRILL_API_KEY>"`. There's no header auth. No SDK dep.

## Config

```toml
refresh_interval_secs = 60
messages_lookback_days = 14

[[tabs]]
name = "messages"
kind = "messages"

[[tabs]]
name = "templates"
kind = "templates"

[[tabs]]
name = "tags"
kind = "tags"

[[tabs]]
name = "webhooks"
kind = "webhooks"
```

### Tab kinds

| `kind` | What it shows |
|---|---|
| `messages` | Recent transactional sends over the last `messages_lookback_days` days (default 14), most-recent-first. Color-coded by delivery state. |
| `templates` | Every template, with current publish state + display subject. |
| `tags` | Every tag with cumulative stats + a derived bounce rate. |
| `webhooks` | Every webhook with subscribed events + trailing chars of the auth key. |

## Layout

- **Tab strip:** one tab per `[[tabs]]` entry, with per-tab count badge. `(N+)` means the API returned more than the v0.1 cap (500 items) and the list was truncated.
- **Items table (left, 45%):**
  - **Messages:** `<subject>  <MM-DD HH:MM>  <to>  · <state>`. Color cues — `delivered` green; `queued` / `scheduled` / `deferred` yellow; `bounced` / `soft-bounced` / `rejected` / `spam` / `unsub` red; `sent` gray.
  - **Templates:** `<name>  [<publish_state>] <subject>`. Published green, draft gray.
  - **Tags:** `<tag>  <sent> sent · <bounce%>`. Bounce rate ≥5% red, ≥2% yellow.
  - **Webhooks:** `<url>  <events> · key <…XXXX>`. Webhooks with `last_error` set are red.
- **Detail panel (right, 55%):** focused item's full detail.
  - **Message:** subject, ID, state, to / from, sent ts, opens, clicks, tags, hint to press `L`.
  - **Template:** name, slug, subject, publish state, timestamps, from-email, labels, first 8 lines of HTML preview.
  - **Tag:** sent, hard / soft bounces, rejects, complaints, unsubs, opens, clicks, unique counts, reputation, derived bounce rate.
  - **Webhook:** id, url, description, auth-key tail, events, batch / event counts, timestamps, last error.

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` / `o` | Open in Mandrill web UI (per-message activity page · per-template code page · per-tag stats page · webhooks settings page) |
| `y` | Yank — message ID, template slug, tag name, webhook URL |
| `L` | For messages, render the full event log (subject, recipient, SMTP events, opens, clicks) into a scratch file under `$TMPDIR` and open it in `$PAGER` (default `less`). |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## API endpoints used

| Tab / action | Endpoint |
|---|---|
| `messages` list | `POST /messages/search.json` (query `*`, date-from / date-to over `messages_lookback_days`) |
| `L` event log | `POST /messages/info.json` (smtp_events + opens_detail + clicks_detail) |
| `templates` list | `POST /templates/list.json` |
| `tags` list | `POST /tags/list.json` |
| `webhooks` list | `POST /webhooks/list.json` |
| `--check` liveness | `POST /users/ping.json` |

## Pagination

v0.1 caps each list at **500 items** and asks for 100 messages per request. When the cap is hit, the tab badge shows `(N+)` so you know the list was truncated. Real cursor pagination is on the v0.2 list.

## Web URL routing

| `o` action | URL |
|---|---|
| Message | `https://mandrillapp.com/activity?details=<id>` |
| Template | `https://mandrillapp.com/templates/code?id=<slug>` |
| Tag | `https://mandrillapp.com/tags/info?tag=<name>` |
| Webhook | `https://mandrillapp.com/settings/webhooks` |

## Run modes

### Standalone

```sh
mnml-msg-mandrill
```

### Blit-host (hosted by mnml)

Not yet — v0.1 is standalone-only. v0.2 will add the `--blit <socket>` mode so mnml can launch it as a native pane (the same shape the AWS family already supports). Until then, run it in a sibling tmnl tab.

## Wire it into mnml's left rail

`mnml-msg-mandrill` ships as a default chip in mnml's `> INTEGRATIONS` rail once blit-host mode lands. For v0.1, the standalone binary is on `$PATH` after `cargo install` and mnml's integration overlay picks it up via binary detection.

## Not yet supported

Held back for v0.2+:

- **Sending email** (`POST /messages/send.json` and `/messages/send-template.json`). This binary is read-only — actual outbound mail goes through your application.
- **Template create / update / publish / delete** (`/templates/{add,update,publish,delete}.json`).
- **Webhook create / update / delete** (`/webhooks/{add,update,delete}.json`).
- **Tag delete** (`/tags/delete.json`).
- **Sender / domain / IP / subaccount management.**
- **Blit-host pane mode** so mnml can host it as a native pane (v0.2 priority).
- **Cursor pagination** — v0.1 caps lists at 500 and surfaces a `(N+)` hint.
- **Per-message attachment download.**

## Security note

The Mandrill API key has full read access (and, on a non-read-only key, full *send* access) to your transactional email — protect `MANDRILL_API_KEY` like a password. Prefer a **read-only key** for `mnml-msg-mandrill` since none of the v0.1 endpoints need write scope. Never commit the key to the TOML config; this binary intentionally only reads it from the env.

## Status

**v0.1** — messages / templates / tags / webhooks tabs, color-coded by state, detail pane, web-console open, ID yank, event-log dump-to-pager. Standalone only.

## Source

[github.com/chris-mclennan/mnml-msg-mandrill](https://github.com/chris-mclennan/mnml-msg-mandrill). MIT.
