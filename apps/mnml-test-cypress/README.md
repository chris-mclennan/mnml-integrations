# mnml-test-cypress

Cypress test results viewer for [mnml](https://mnml.sh) — a
terminal TUI for browsing mochawesome JSON reports. Pass/fail
state per test, filter to failures, yank the spec file path.
Runs standalone in any terminal or as a hosted mnml pane.

Part of the family `mnml-test-*` siblings — sibling to
[mnml-test-playwright](https://github.com/chris-mclennan/mnml-test-playwright).
The **test runner** stays in mnml core (runs on the buffer you
have open, jumps to source on accept); this sibling is the
read-only results inspector.

```
┌─ /path/to/cypress/results/mochawesome.json ──────────────────────┐
│ 🧪 cypress  · 47p / 3f / 0pending · 12.4s                         │
└──────────────────────────────────────────────────────────────────┘
┌─ 5 rows ─────────────────────────────────────────────────────────┐
│ ▸ 📄 login.cy.js  (8p, 2f)                                        │
│    ✗  Login flow › rejects bad password                  2.5s    │
│    ✗  Login flow › times out on invalid creds            30.1s   │
│   📄 checkout.cy.js  (12p, 1f)                                    │
│    ✗  Checkout › card declined                           1.8s    │
└──────────────────────────────────────────────────────────────────┘
┌─ failure ────────────────────────────────────────────────────────┐
│ expected URL to contain "/dashboard", got "/login"                │
│   AssertionError: at chai.assertion                               │
│     at ...                                                        │
└──────────────────────────────────────────────────────────────────┘
  47p / 3f / 0pending · 5/50 rows · filter: failures only
  ↑↓/jk · F failures-only · y yank spec · r reload · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-test-cypress mnml-test-cypress
mnml-test-cypress --install
```

## Usage

```sh
# Point at a mochawesome JSON file
mnml-test-cypress cypress/results/mochawesome.json

# Or at a directory — auto-finds mochawesome.json / output.json /
# results/mochawesome.json inside it
mnml-test-cypress cypress/results/

# Print parsed stats without launching the TUI
mnml-test-cypress cypress/results/mochawesome.json --check
```

The path is positional. v0.1 reads only [mochawesome](https://github.com/adamgruber/mochawesome) JSON (the default Cypress JSON reporter doesn't carry enough structure to render usefully). Wire your `cypress.config.js` like:

```js
const { defineConfig } = require("cypress");

module.exports = defineConfig({
  reporter: "mochawesome",
  reporterOptions: {
    reportDir: "cypress/results",
    overwrite: false,
    html: false,
    json: true,
  },
});
```

Then `npx cypress run` writes `cypress/results/mochawesome.json` after the suite finishes; point this viewer at that file.

## Keys

| Chord | Action |
|---|---|
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Page up / down |
| `g` / `G` | Top / bottom |
| `F` | Toggle "failures only" / "all" filter |
| `y` | Yank focused row's spec file path (absolute) to OS clipboard |
| `r` | Reload mochawesome JSON from disk (re-run cypress, then `r`) |
| `q` / `Esc` / `Ctrl+C` | Quit |

Default filter is **failures only** — most of the time you open a results viewer because something broke. If there are zero failures, the viewer falls back to showing all tests on open.

## Layout

- **Header:** path to the loaded JSON + run stats (passes / failures / pending / total duration)
- **Body:** flat row list — spec headers (`📄 file.cy.js (Np, Nf)`) followed by their tests. Failed tests are red + bold; passes green; pending yellow.
- **Failure detail (when a failed row is selected):** error message + first 4 lines of stack
- **Status line:** active filter, row counts, key hints

## Use it as an mnml pane

`mnml-test-cypress` speaks the `tmnl-protocol` blit-host shape when launched with `--blit <socket>`. mnml can host it inside a regular pane:

```vim
:host.launch mnml-test-cypress cypress/results/mochawesome.json
```

The positional path is passed through verbatim — useful for wiring it into a tmnl chord or palette command that opens a fresh results file on demand.

## Status

**v0.1 (this release)** — Mochawesome JSON parsing, flat test list with filter, spec-path yank, error details panel. Standalone TUI + blit-host mode.

Held back for v0.2+:
- Screenshot rendering inline (Cypress writes failure screenshots to `cypress/screenshots/`; v0.2 would link from a failed row to the screenshot path and use mnml's image protocol to preview it inline)
- Video link (Cypress's `cypress/videos/<spec>.mp4` — would yank or open in default player)
- Tree-view layout grouping by suite (currently flat, with suite path joined into the test title)
- Other Cypress reporter formats (junit XML, default JSON) — v0.1 is mochawesome-only because that's the format with enough structure to render usefully
- The `tmnl-protocol::Message::OpenFile { path }` integration for "Enter on a row → open spec at the failing line in mnml's editor"

## Source

The viewer lives in its own sibling repo: [github.com/chris-mclennan/mnml-test-cypress](https://github.com/chris-mclennan/mnml-test-cypress). MIT-licensed. See [Building integrations](https://mnml.sh/manual/integrations/building/) for the anatomy of an integration.

## License

MIT.
