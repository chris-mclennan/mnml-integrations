# mnml-fs-s3

Amazon S3 browser for [mnml](https://mnml.sh) — terminal TUI for
browsing buckets + prefixes, downloading objects, and yanking
URIs. Runs standalone in any terminal or as a hosted mnml pane.
Shells out to the `aws` CLI; no SDK dependency.

The first of the family's `mnml-fs-*` siblings — opens the door
to `mnml-fs-gcs` (Google Cloud Storage), `mnml-fs-azureblob`,
etc. with the same TUI shape.

```
┌─ s3 ─────────────────────────────────────────────────────────────┐
│ ▸1.logs  2.exports  3.configs                                     │
└──────────────────────────────────────────────────────────────────┘
┌─ logs ───────────────────────────────────────────────────────────┐
│ 📁 my-app-logs / 2026 / 06                                        │
└──────────────────────────────────────────────────────────────────┘
┌─ 12 entries ─────────────────────────────────────────────────────┐
│ ▸ 📁 errors/                                                      │
│   📁 access/                                                      │
│   📄 build-log.txt              1.2 MB    2026-06-06              │
│   📄 application.log            45 KB     2026-06-06              │
│   📄 deploy.json                2.4 KB    2026-06-06              │
│   …                                                               │
└──────────────────────────────────────────────────────────────────┘
  ↑↓/jk · Enter open · BS up · y URI · Y presign · o console · d del · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-fs-s3 mnml-fs-s3
mnml-fs-s3 --install
```

You'll also need the [AWS CLI](https://aws.amazon.com/cli/) on
your `$PATH` with credentials configured (`aws configure` or any
of the usual environment variables / shared-credentials files).

## Setup

1. **Verify the AWS CLI works.** Whatever you'd run from your
   shell — `aws s3 ls`, `aws sts get-caller-identity` — needs to
   succeed before this viewer can. There's no separate
   credential chain.

2. **Run once** to scaffold the config template:

   ```sh
   mnml-fs-s3
   ```

   Writes `~/.config/mnml-fs-s3.toml`. Edit the `[[buckets]]`
   list — one entry per bucket you want as a tab.

3. **Re-run** — the TUI launches with your configured tabs.

4. **Verify** the resolved config + AWS CLI state without
   launching the TUI:

   ```sh
   mnml-fs-s3 --check
   ```

## Config

```toml
# Optional global:
#   refresh_interval_secs — default 0 (no auto-refresh).
#   S3 listings don't churn, so the default is no-poll;
#   press `r` in the TUI to refresh.

refresh_interval_secs = 0

# ── Buckets ──────────────────────────────────────────────────────
# Each [[buckets]] entry is one tab. Switch with 1-9 in the TUI.

[[buckets]]
name = "logs"
bucket = "my-app-logs"
prefix = "2026/"            # optional starting prefix
# region = "us-east-1"      # optional; defaults to AWS CLI's region

[[buckets]]
name = "exports"
bucket = "my-data-exports"

[[buckets]]
name = "configs"
bucket = "my-app-configs"
prefix = "prod/"
```

`bucket` is the bare name (`my-app-logs`, not `s3://my-app-logs/`).
`prefix` jumps you straight into a subtree. Region defers to the
AWS CLI by default.

## Auth shape

There is none — at least, not on this viewer's side. Every S3
operation is a subprocess call to `aws s3` / `aws s3api`. The
AWS CLI's own credential chain (env vars → shared credentials →
SSO → instance role) is what authenticates the call. That means:

- `AWS_PROFILE`, `AWS_REGION`, `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY` set in your shell flow through.
- `aws sso login` sessions just work — the viewer doesn't
  manage tokens.
- Multi-account setups: switch profiles before launching;
  the active profile is the one queried.

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that bucket tab |
| `Tab` / `BackTab` | Cycle tabs forward / back |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` | On a prefix → drill in. On a file → download to `~/.cache/mnml-fs-s3/<bucket>/<key>` |
| `Backspace` / `h` | Up one prefix level |
| `y` | Yank `s3://bucket/key` URI to OS clipboard |
| `Y` | Yank presigned URL (5-min TTL) to OS clipboard |
| `o` | Open S3 console URL in browser (anchored at current prefix) |
| `d` | Delete focused object (asks for `y` to confirm) |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## File-open handoff — v0.1, v0.2, v0.3

This is the interesting integration point. There are three levels;
v0.1 ships the simple one and notes the rest.

**v0.1 (this release):** Press `Enter` on a file → sibling
downloads to `~/.cache/mnml-fs-s3/<bucket>/<key>` → status shows
the local path. User copies the path manually (or `y`-yanks the
`s3://` URI for later) and opens it however they like. Simple,
works today, no protocol changes.

**v0.2 (planned):** When running as a hosted pane
(`:host.launch mnml-fs-s3`), the sibling emits a
`tmnl-protocol::Message::OpenFile { path }` event after download.
mnml-as-host picks it up and opens the file in its editor pane.
Sibling stays focused; you `Tab` between editor + S3 browser.
Same protocol change benefits future siblings.

**v0.3 (later):** Save-back. Remember the (bucket, key) → local
path mapping. Add a save-hook in mnml core that calls the
sibling when a file from `~/.cache/mnml-fs-s3/` is saved.
Sibling does `aws s3 cp` upload. Now you can actually edit
configs in S3 from mnml.

## Two run modes

### Standalone

Just run `mnml-fs-s3` in any terminal. The TUI takes over until
you `q`.

### Blit-host (hosted by mnml)

```vim
:host.launch mnml-fs-s3
```

mnml spawns it with `--blit <socket>` and renders the streamed
cells into a native `Pane::BlitHost`. The pane becomes a normal
mnml pane — splittable, focusable, key-routed. `Ctrl+E` releases
focus back to the layout tree. See [Building
integrations](https://mnml.sh/manual/integrations/building/) for
the protocol mechanism.

## Wire it into mnml's left rail

If you want a one-click chip in mnml's rail that opens the S3
viewer, drop this into your `~/.config/mnml/config.toml`:

```toml
[[ui.integration_icon]]
id       = "s3"
glyph    = "\U000F0EBC"            # nf-md-aws (TOML 8-digit form)
fallback = "S3"
command  = ":host.launch mnml-fs-s3"
color    = "orange"
tooltip  = "Open S3 browser"
```

Setting `[[ui.integration_icon]]` **replaces** the built-in
defaults, so copy the defaults from `mnml/src/config.rs` into
your config first if you want to extend rather than replace.

## What stays out of v0.1

The TUI is intentionally minimal. Held back for v0.2+:

- Upload prompt (the `u` key — the underlying `aws s3 cp` call
  is implemented; the prompt UI is what's deferred)
- Multi-bucket parallel listing
- Glacier / IA tier visibility (column hidden by default)
- Versioning support (latest only)
- Encryption metadata
- Recursive operations (download whole prefix as zip)
- Multi-select for batch ops
- The OpenFile blit-host event (see "File-open handoff" above)

## Status

**v0.1 (this release)** — Bucket tabs, prefix navigation,
download to cache, URI yank, presigned URL yank, S3 console
open, delete with confirmation. `aws` CLI shell-out auth.
Standalone TUI + blit-host mode.

## Source

The viewer lives in its own sibling repo:
[github.com/chris-mclennan/mnml-fs-s3](https://github.com/chris-mclennan/mnml-fs-s3).
MIT-licensed. See
[CONTRIBUTING.md](CONTRIBUTING.md) for fork + PR guidance.

## License

MIT.
