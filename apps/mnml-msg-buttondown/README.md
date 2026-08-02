# mnml-msg-buttondown

A terminal browser for [Buttondown](https://buttondown.email/) — list drafts, browse sent newsletter issues with open/click stats, peek at the scheduled queue, and manage subscribers. The first **messaging** sibling in the mnml family to wrap a newsletter platform; sits next to `mnml-msg-slack` and `mnml-msg-teams`.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane.

```
┌─ buttondown ──────────────────────────────────────────────────────────┐
│ ▸1.drafts (3)  2.sent (47)  3.scheduled (1)  4.subscribers (1248+)    │
└───────────────────────────────────────────────────────────────────────┘
┌─ drafts (3) ──────────────────┐ ┌─ detail ────────────────────────────┐
│ ▸ Issue #48 — half-baked      │ │ Subject     Issue #48 — half-baked  │
│   Reply to last week          │ │ ID          abc-123                 │
│   Untitled scratch            │ │ Status      draft                   │
│                               │ │ Created     2026-06-05T00:00:00Z    │
│                               │ │ Word count  812                     │
│                               │ │                                     │
│                               │ │  Body                               │
│                               │ │  # heading                          │
│                               │ │  draft body content here…           │
└───────────────────────────────┘ └─────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · o web · y ID · p publish · X unsubscribe · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-msg-buttondown
mnml-msg-buttondown --install
```

## Setup

1. **Auth (env var).** Buttondown uses a single API key.
   ```sh
   export BUTTONDOWN_API_KEY=...   # Buttondown → Settings → Programming
   ```
2. **Run once** to scaffold the config:
   ```sh
   mnml-msg-buttondown
   ```
3. **Edit** `~/.config/mnml-msg-buttondown/config.toml` if you want to drop or reorder tabs.
4. **Re-run.**

`mnml-msg-buttondown --check` prints the resolved config + whether the env var is set + the API base URL.

## Auth shape

Plain HTTP — every request carries `Authorization: Token <BUTTONDOWN_API_KEY>` and hits `https://api.buttondown.email/v1/...`. No SDK dep.

**Security note:** the Buttondown API key grants full newsletter access — read, schedule, and delete subscribers — so treat it like a password. The TUI never logs or echoes the key; `--check` only prints its length and last four characters.

## Config

```toml
refresh_interval_secs = 60

[[tabs]]
name = "drafts"
kind = "drafts"

[[tabs]]
name = "sent"
kind = "sent"

[[tabs]]
name = "scheduled"
kind = "scheduled"

[[tabs]]
name = "subscribers"
kind = "subscribers"
```

### Tab kinds

| `kind` | What it shows |
|---|---|
| `drafts` | Unsent drafts (`GET /emails?status=draft`). `p` schedules the focused draft for 5 minutes from now. |
| `sent` | Already-shipped emails (`GET /emails?status=sent`), with open / click counts when Buttondown reports them. |
| `scheduled` | Emails queued for a future send (`GET /emails?status=scheduled`). Publish date highlighted. |
| `subscribers` | Every subscriber (`GET /subscribers`), color-coded by type. `X` unsubscribes the focused one. |

## Layout

- **Tab strip:** one tab per `[[tabs]]` entry with a per-tab count badge. `(N+)` means Buttondown reported more results than we received on page 1 (v0.1 doesn't paginate — bumping past page 1 is on the v0.2 list).
- **Items table (left, 45%):**
  - **drafts:** `<subject>  <created> · <wordcount>w`.
  - **sent:** `<subject>  <publish> · <type> · <opens>o/<clicks>c`.
  - **scheduled:** `<subject>  ⏰ <publish>`.
  - **subscribers:** `<email>  <type> · <created> · <notes>`. Color cues — `premium` yellow, `regular` green, `unactivated` / unknown gray, `unsubscribed` / `removed` red.
- **Detail panel (right, 55%):** focused item's full detail.
  - **Email:** subject, id, status, type, creation / publish / modification dates, word count, send stats (recipients / opens / clicks when present), the full body (first 40 lines — Markdown rendered as plain text in v0.1).
  - **Subscriber:** email, id, type, creation date, source, free-form notes, metadata bag (pretty-printed JSON).

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` / `o` | Open in Buttondown web UI (emails → `/emails/{id}`, subscribers → `/subscribers/{id}`) |
| `y` | Yank — copy the focused item's id |
| `p` | **Publish a draft.** Drafts tab only. `PATCH /emails/{id}` with `status=scheduled` and `publish_date=<5 min from now>`. Confirms with `[y/n]`. v0.2 will add a date picker. |
| `X` | **Unsubscribe.** Subscribers tab only. `DELETE /subscribers/{id}`. Confirms with `[y/n]`. |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit (`Esc` cancels a pending `[y/n]` prompt first) |

## API endpoints used

| Tab / action | Endpoint |
|---|---|
| `drafts` | `GET /emails?status=draft&page=1` |
| `sent` | `GET /emails?status=sent&page=1` |
| `scheduled` | `GET /emails?status=scheduled&page=1` |
| `subscribers` | `GET /subscribers?page=1` |
| `p` publish | `PATCH /emails/{id}` (`status=scheduled`, `publish_date=...`) |
| `X` unsubscribe | `DELETE /subscribers/{id}` |

## Errors + rate limits

Buttondown surfaces errors as:

- 4xx with `{"detail": "..."}` — surfaced verbatim as `buttondown: <detail>`
- Validation errors with `{"non_field_errors": [...]}` or per-field arrays — the first message is surfaced
- Buttondown limits ~600 req/min; this TUI never auto-retries on `429` — refresh manually with `r`

## Pagination

v0.1 fetches **page 1 only** for each tab (Buttondown's default page size is 100). When the API reports a `count` larger than what we received, the tab badge shows `(N+)` so you know there's more. Real pagination (continuing past page 1) is on the v0.2 list.

## Run modes

### Standalone

```sh
mnml-msg-buttondown
```

### Blit-host (hosted by mnml)

Not yet — v0.1 is standalone-only. v0.2 will add the `--blit <socket>` mode so mnml can launch it as a native pane (the same shape the AWS family already supports). Until then, run it in a sibling tmnl tab.

## Wire it into mnml's left rail

`mnml-msg-buttondown` will ship as a default chip in mnml's rail under **INTEGRATIONS** once blit-host mode lands. For v0.1, the standalone binary is on `$PATH` after `cargo install` and the integration overlay picks it up.

## Not yet supported

Held back for v0.2+:

- **Composing new drafts** — v0.1 is read-only-ish; create drafts in the Buttondown web editor for now.
- **Scheduled-send picker** — `p` ships at "5 minutes from now"; v0.2 will prompt for a date/time.
- **Tag management** — list/edit subscriber tags + segment-based sends.
- **Analytics deep-dive** — per-issue link click-through, geographic open distribution, A/B test results.
- **Automations editor** — sequence drips, welcome series, etc.
- **Cursor pagination** — v0.1 stops at page 1 and surfaces a `(N+)` hint.
- **Survey / poll responses.**
- **Image upload / asset management.**
- **Blit-host pane mode** so mnml can host it as a native pane (the v0.1 priority follow-up).

## Status

**v0.1** — drafts / sent / scheduled / subscribers tabs, color-coded by subscriber type, detail pane with email body + subscriber metadata, web open, id yank, publish-draft action with `[y/n]` confirm, unsubscribe action with `[y/n]` confirm. Standalone only.

## Source

[github.com/chris-mclennan/mnml-msg-buttondown](https://github.com/chris-mclennan/mnml-msg-buttondown). MIT.
