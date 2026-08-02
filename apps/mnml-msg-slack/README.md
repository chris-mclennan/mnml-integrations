# mnml-msg-slack

A terminal browse + post client for [Slack](https://slack.com) — list your channels and DMs, peek the most-recent 30 messages in any channel, run interactive search, post messages, reply in threads, react with a quick-pick of common emojis, and copy permalinks to the clipboard. The first **messaging** sibling in the mnml family.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane (see [TODO](#not-yet-supported) below).

```
┌─ slack — Acme ───────────────────────────────────────────────────────────┐
│ ▸1.channels (37)  2.dms (12)  3.search (0)  4.threads                    │
└──────────────────────────────────────────────────────────────────────────┘
┌─ channels (37) ───────────────┐ ┌─ #general ──────────────────────────┐
│ ▸ #general                    │ │ 09:14:22 chrism        morning team │
│   #announcements              │ │ 09:18:01 alice         heads up...  │
│   #eng-platform               │ │ 09:24:33 bob           ↳3 thread    │
│   #random                     │ │ 09:42:11 carol         shipped 1.2  │
│   …                           │ │ …                                   │
└───────────────────────────────┘ └─────────────────────────────────────┘
 1-9 tab · ↑↓/jk move · Enter open · / search · p post · R react · T thread · y permalink · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-msg-slack
mnml-msg-slack --install
```

## Setup

Slack tokens live behind app creation, not a settings dialog. Five steps:

1. Visit **<https://api.slack.com/apps>** and click **Create New App → From scratch**. Pick any name; choose the workspace you want to browse.
2. In the left rail, open **OAuth & Permissions** and scroll to **Scopes → User Token Scopes**. Add all of:

   | Scope | Why |
   |---|---|
   | `channels:read` | list public channels |
   | `channels:history` | read public channel messages |
   | `groups:read` | list private channels |
   | `groups:history` | read private channel messages |
   | `im:read` | list direct messages |
   | `im:history` | read direct messages |
   | `mpim:read` | list group DMs |
   | `mpim:history` | read group DMs |
   | `search:read` | run search.messages |
   | `chat:write` | post + reply in threads |
   | `reactions:read` | read reactions on messages |
   | `reactions:write` | add reactions |
   | `users:read` | resolve user IDs to display names |

3. Scroll up to **OAuth Tokens for Your Workspace → Install to Workspace**. Approve the scope request.
4. Copy the **User OAuth Token** (starts with `xoxp-…`).
5. Export it and run:

   ```sh
   export SLACK_USER_TOKEN=xoxp-...
   mnml-msg-slack --check
   ```

   `--check` prints the resolved config + the result of an `auth.test` round-trip so you can verify the token is good before launching the UI.

The optional `SLACK_BOT_TOKEN` (xoxb-…) is recognized as a fallback but v0.1 prefers the user token (some endpoints — notably `search.messages` — only work with user tokens).

## Config

`~/.config/mnml-msg-slack/config.toml` (scaffolded on first run):

```toml
refresh_interval_secs = 60
post_multiline = false

[[tabs]]
name = "channels"
kind = "channels"

[[tabs]]
name = "dms"
kind = "dms"

[[tabs]]
name = "search"
kind = "search"

[[tabs]]
name = "threads"
kind = "threads"
```

### Tab kinds

| `kind` | What it shows |
|---|---|
| `channels` | Public + private channels you're a member of (members highlighted, non-members dimmed) |
| `dms` | 1:1 DMs + multi-person group DMs |
| `search` | Interactive `search.messages` query (`/` to enter or update) |
| `threads` | v0.1 stub — populated in v0.2 |

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` | Open thread view (v0.1: reloads detail pane history) |
| `/` | Search input bar — submit with `Enter`, cancel with `Esc` |
| `p` | Post mode — type a message, `Enter` sends `chat.postMessage` to the focused channel |
| `R` | Reaction picker — `←→/hjkl` to pick one of 12 quick emojis, `Enter` to react |
| `T` | Thread reply — same as post mode but `thread_ts` is the focused message |
| `y` | Yank permalink for the focused message / search hit |
| `r` | Force-refresh active tab (bypasses the 5-min channel-list cache) |
| `q` / `Esc` / `Ctrl+C` | Quit |

In any input bar, `Backspace` deletes a character and `Ctrl+C` cancels.

## API endpoints used

| Where | Endpoint |
|---|---|
| Startup + `--check` | `GET /auth.test` |
| `channels` / `dms` tabs | `GET /conversations.list?types=...&exclude_archived=true&limit=200` |
| Detail pane (channel history) | `GET /conversations.history?channel=...&limit=30` |
| User-name resolution | `GET /users.info?user=...` (lazy, per unknown id) |
| `search` tab | `GET /search.messages?query=...&count=50&sort=timestamp&sort_dir=desc` |
| `p` / `T` (post + thread reply) | `POST /chat.postMessage` (form-encoded) |
| `R` (react) | `POST /reactions.add` (form-encoded) |
| `y` (permalink) | `GET /chat.getPermalink?channel=...&message_ts=...` |

## Caching + rate limits

- `conversations.list` is cached in-memory for 5 minutes so tab switches don't hammer Slack's Tier 3 rate limit (~50 req/min). `r` forces a refresh.
- User-id → display-name lookups are cached for the session (lazy on first sight).
- On HTTP 429, the status bar reports `slack: rate-limited, retry in <N>s` (read from `Retry-After`). v0.1 does not auto-retry.

## Not yet supported

Held back for v0.2+:

- **File uploads** (`files.uploadV2`) — handled separately because the API is multipart + two-step.
- **Edit / delete your own messages** (`chat.update`, `chat.delete`).
- **Status updates** (`users.profile.set`).
- **Workspace switching** (run with a different `SLACK_USER_TOKEN` to switch).
- **Workflows** (`workflows.*`).
- **Thread tab auto-population** — needs a periodic scan across recently-active channels for unread thread replies.
- **In-pane threaded reader** — v0.1 shows the latest 30 channel messages flat; threading visualisation is v0.2.
- **Blit-host pane mode** so mnml can host it as a native pane (the v0.1 priority follow-up).
- **Live-tail of channel history** — v0.1 re-fetches on selection move and every `refresh_interval_secs`. WebSocket / Events API is v0.3.

## Security notes

The user token (`xoxp-…`) has broad access — anything **you** can read and write, the token can. Treat it like a password:

- Store it in 1Password / your OS keychain, not in a dotfile checked into git.
- Don't share `--check` output with the token unmasked (mnml-msg-slack masks it for you, but `env` and shell history won't).
- Revoke unused tokens at **<https://api.slack.com/apps>** → your app → **OAuth & Permissions → Revoke Tokens**.

## Source

[github.com/chris-mclennan/mnml-msg-slack](https://github.com/chris-mclennan/mnml-msg-slack). MIT.
