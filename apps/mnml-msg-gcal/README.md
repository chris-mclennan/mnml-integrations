# mnml-msg-gcal

Terminal Google Calendar client for the [mnml](https://mnml.sh) family — a
lightweight ratatui TUI that surfaces today's meetings, the week ahead, and
upcoming events. Quick-create dialogs, keyboard-driven navigation, respects
the calendar list you've subscribed to in the Google web app.

Runs standalone in any terminal, or hosted as an mnml Pty pane (`<leader>iC`
after `mnml-msg-gcal --install`).

## Status — v0.1 skeleton

**Working:**
- CLI + config layer (`--check` prints resolved config + OAuth setup hints)
- `--install` / `--uninstall` (registers with mnml via `mnml-bridge` 0.3
  integration manifest)
- Calendar API v3 client types + `list_events` (`gcal.rs`)
- Config scaffolding at `~/.config/mnml-msg-gcal.toml`
- OAuth token cache format (`auth.rs`)

**Not yet implemented (see TODO markers):**
- OAuth interactive loopback flow (browser → local HTTP → code exchange)
- TUI event loop (ratatui Terminal + key dispatch)
- Quick-create event popup

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-msg-gcal mnml-msg-gcal
mnml-msg-gcal --install     # register with mnml
```

Once registered, mnml (0.2+) picks up the rail chip + `gcal.open` palette
command + `<leader>iC` chord on next restart (or after
`integrations.refresh`).

## Setup (per-user GCP project)

Google Calendar API requires a per-user OAuth client — same shape
`gcloud auth login` uses.

1. Open <https://console.cloud.google.com>, create a new project (or reuse
   one).
2. Enable **Calendar API v3** under *APIs & Services → Library*.
3. **OAuth consent screen** — pick *External* + fill in the required fields
   (app name, support email). Add your own email as a *Test user*.
4. **Credentials → Create Credentials → OAuth Client ID → Desktop app**.
   Save the client_id + client_secret.
5. Drop them into `~/.config/mnml-msg-gcal/client.toml`:

   ```toml
   client_id     = "<your-client-id>.apps.googleusercontent.com"
   client_secret = "<your-client-secret>"
   ```

6. Run `mnml-msg-gcal --check` to verify.

Once the OAuth loopback flow lands in v0.2, the first `mnml-msg-gcal`
launch will open your browser, you'll grant Calendar scope, and the token
lands at `~/.config/mnml-msg-gcal/token.json`.

## Config

`~/.config/mnml-msg-gcal.toml`:

```toml
calendar_id  = "primary"    # or an email-shaped calendar id
timezone     = "America/New_York"
refresh_secs = 60
upcoming_days = 14
```

## Keys (planned for v0.2)

| Key | Action |
|---|---|
| `1` / `2` / `3` | Switch to Today / Week / Upcoming |
| `j` / `↓`, `k` / `↑` | Move selection |
| `Enter` | Open event details |
| `n` | Quick-create event |
| `r` | Refresh |
| `y` | Yank event link (`htmlLink`) |
| `q` / `Esc` / `Ctrl+C` | Quit |

## License

MIT.
