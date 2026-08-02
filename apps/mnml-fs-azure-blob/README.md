# mnml-fs-azure-blob

Azure Blob Storage browser for [mnml](https://mnml.sh) — terminal
TUI for browsing storage accounts, containers, and blobs;
downloading objects; yanking https URIs and SAS URLs. Runs
standalone in any terminal or as a hosted mnml pane. Shells out
to the `az` CLI; no SDK dependency.

Sibling of [`mnml-fs-s3`](https://github.com/chris-mclennan/mnml-fs-s3) —
same TUI shape, same chord set, different cloud.

```
┌─ Azure Blob ─────────────────────────────────────────────────────┐
│ ▸1.accounts  2.logs  3.exports                                    │
└──────────────────────────────────────────────────────────────────┘
┌─ logs ───────────────────────────────────────────────────────────┐
│ 📁 mystorageacct / logs / 2026 / 06                               │
└──────────────────────────────────────────────────────────────────┘
┌─ 12 entries ─────────────────────────────────────────────────────┐
│ ▸ 📁 errors/                                                      │
│   📁 access/                                                      │
│   📄 build-log.txt              1.2 MB    2026-06-06              │
│   📄 application.log            45 KB     2026-06-06              │
│   📄 deploy.json                2.4 KB    2026-06-06              │
│   …                                                               │
└──────────────────────────────────────────────────────────────────┘
  ↑↓/jk · Enter open · BS up · y URI · Y SAS · o portal · d del · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-fs-azure-blob mnml-fs-azure-blob
mnml-fs-azure-blob --install
```

You'll also need the [Azure CLI](https://learn.microsoft.com/cli/azure/install-azure-cli)
on your `$PATH`, signed in (`az login`) before launching the viewer.
There's no separate credential chain.

## Setup

1. **Verify the Azure CLI is logged in.** Whatever you'd run from
   your shell — `az storage account list`, `az account show` — needs
   to succeed before this viewer can.

   ```sh
   az login            # interactive browser login
   az account show     # confirm subscription
   ```

2. **Run once** to scaffold the config template:

   ```sh
   mnml-fs-azure-blob
   ```

   Writes `~/.config/mnml-fs-azure-blob.toml`. Edit the `[[tabs]]`
   list — one entry per view you want as a tab.

3. **Re-run** — the TUI launches with your configured tabs.

4. **Verify** the resolved config + Azure CLI state without
   launching the TUI:

   ```sh
   mnml-fs-azure-blob --check
   ```

## Config

```toml
# Optional global:
#   refresh_interval_secs — default 0 (no auto-refresh).
#   Blob listings don't churn, so the default is no-poll;
#   press `r` in the TUI to refresh.

refresh_interval_secs = 0

# ── Tabs ─────────────────────────────────────────────────────────
# Each [[tabs]] entry is one tab. Switch with 1-9 in the TUI.
#
# `kind` is one of:
#   "accounts"   : list every storage account in your subscription
#   "containers" : list containers in a named storage account
#   "blobs"      : list blobs in a named container (optional prefix)

[[tabs]]
name = "all accounts"
kind = "accounts"

[[tabs]]
name = "logs"
kind = "blobs"
account = "mystorageacct"
container = "logs"
# prefix = "2026/"             # optional starting prefix

[[tabs]]
name = "exports"
kind = "containers"
account = "mystorageacct"
```

`account` is the bare storage-account name (`mystorageacct`, not
the full `https://mystorageacct.blob.core.windows.net/` URL).
`prefix` jumps you straight into a subtree (trailing `/` matters).

## Auth shape

There is none — at least, not on this viewer's side. Every Azure
operation is a subprocess call to `az storage`. The Azure CLI's
own credential chain (interactive `az login`, env vars,
managed-identity, service-principal) is what authenticates the
call. That means:

- `az login` sessions just work — the viewer doesn't manage
  tokens.
- Service-principal env vars (`AZURE_CLIENT_ID`,
  `AZURE_TENANT_ID`, `AZURE_CLIENT_SECRET`) flow through.
- Multi-subscription setups — switch with `az account set
  --subscription <name>` before launching.
- `--auth-mode login` is passed on every blob op, so AAD RBAC
  applies. Listing containers needs the **Storage Blob Data
  Reader** role; downloading and deleting need **Contributor**
  on the account.

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs forward / back |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` | Drill in: account → containers, container → blobs, prefix → deeper prefix, blob → download to `~/.cache/mnml-fs-azure-blob/<account>/<container>/<blob>` |
| `Backspace` / `h` | Up one view level |
| `y` | Yank `https://<account>.blob.core.windows.net/...` URL to OS clipboard |
| `Y` | Yank SAS-signed read-only URL (5-min TTL) to OS clipboard |
| `o` | Open Azure Portal URL for the focused row in browser |
| `d` | Delete focused blob (asks for `y` to confirm) |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## File-open handoff — v0.1, v0.2, v0.3

This is the interesting integration point. There are three levels;
v0.1 ships the simple one and notes the rest.

**v0.1 (this release):** Press `Enter` on a blob → sibling
downloads to `~/.cache/mnml-fs-azure-blob/<account>/<container>/<blob>`
→ status shows the local path. User opens it however they like
(or `y`-yanks the URL for later). Simple, works today, no protocol
changes.

**v0.2 (planned):** When running as a hosted pane
(`:host.launch mnml-fs-azure-blob`), the sibling emits a
`tmnl-protocol::Message::OpenFile { path }` event after download.
mnml-as-host picks it up and opens the file in its editor pane.
Sibling stays focused; you `Tab` between editor + blob browser.

**v0.3 (later):** Save-back. Remember the (account, container,
blob) → local path mapping. Add a save-hook in mnml core that
calls the sibling when a file from `~/.cache/mnml-fs-azure-blob/`
is saved. Sibling does `az storage blob upload` to push the
change back.

## Two run modes

### Standalone

Just run `mnml-fs-azure-blob` in any terminal. The TUI takes over
until you `q`.

### Blit-host (hosted by mnml)

```vim
:host.launch mnml-fs-azure-blob
```

mnml spawns it with `--blit <socket>` and renders the streamed
cells into a native `Pane::BlitHost`. The pane becomes a normal
mnml pane — splittable, focusable, key-routed. `Ctrl+E` releases
focus back to the layout tree. See [Building
integrations](https://mnml.sh/manual/integrations/building/) for
the protocol mechanism.

## Wire it into mnml's left rail

If you want a one-click chip in mnml's rail that opens the Azure
Blob viewer, drop this into your `~/.config/mnml/config.toml`:

```toml
[[ui.integration_icon]]
id       = "azure-blob"
glyph    = "\U000F0805"            # nf-md-microsoft-azure (TOML 8-digit form)
fallback = "Az"
command  = ":host.launch mnml-fs-azure-blob"
color    = "blue"
tooltip  = "Open Azure Blob browser"
```

Setting `[[ui.integration_icon]]` **replaces** the built-in
defaults, so copy the defaults from `mnml/src/config.rs` into
your config first if you want to extend rather than replace.

## What stays out of v0.1

The TUI is intentionally minimal. Held back for v0.2+:

- Upload prompt (the `az storage blob upload` call is implemented
  in the data layer; the prompt UI is what's deferred)
- Multi-account parallel listing
- Archive / cool-tier rehydration affordances
- Versioning + snapshot support (latest only)
- Encryption-scope metadata
- Recursive operations (download whole prefix as zip)
- Multi-select for batch ops
- The OpenFile blit-host event (see "File-open handoff" above)

## Status

**v0.1 (this release)** — Three view kinds (accounts /
containers / blobs), drill-down navigation, download to cache,
https URL yank, SAS URL yank, Azure portal open, blob delete with
confirmation. `az` CLI shell-out auth. Standalone TUI + blit-host
mode.

## Source

The viewer lives in its own sibling repo:
[github.com/chris-mclennan/mnml-fs-azure-blob](https://github.com/chris-mclennan/mnml-fs-azure-blob).
MIT-licensed.

## License

MIT.
