# mnml-test-playwright

Playwright `trace.zip` viewer for [mnml](https://mnml.sh) — a
terminal TUI for the per-action timeline, console messages, and
errors recorded by Playwright. Runs standalone in any terminal or
as a hosted mnml pane.

Part of the family `mnml-test-*` siblings (currently this is the
only one; `mnml-test-jest` / `mnml-test-cypress` / `mnml-test-vitest`
would fit alongside). The Playwright **test runner** stays in mnml
core (editor-integrated — runs on the buffer you have open, jumps
to source on accept); this sibling is just the trace inspector.

```
┌─ trace ──────────────────────────────────────────────────────────┐
│ checkout.spec.ts:24 · filters:  actions  console  errors  stdio  │
└──────────────────────────────────────────────────────────────────┘
┌─ events (47/89) ─────────────────────────────────────────────────┐
│ ▸ ▶    0.012s  page.goto https://shop.example.com                │
│   ▶    1.203s  page.fill #email = user@example.com               │
│   ▶    1.412s  page.click [data-testid="submit"]                 │
│   ●    1.518s  Error: Timeout 30000ms exceeded                   │
│   …                                                              │
└──────────────────────────────────────────────────────────────────┘
  ↑↓/jk · a/c/e/s · E errors-only · R show-all · r reload · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-test-playwright mnml-test-playwright
mnml-test-playwright --install
```

## Usage

```sh
# Open a single trace.zip
mnml-test-playwright path/to/trace.zip

# Print version and exit
mnml-test-playwright --check
```

The path is positional — no config file in v0.1. Each Playwright
test that runs with `trace: 'retain-on-failure'` (or `'on'`) drops
a `trace.zip` in `test-results/<test>/`; point this viewer at that
file.

## Keys

| key            | action                              |
| -------------- | ----------------------------------- |
| `↑` / `k`      | move selection up                   |
| `↓` / `j`      | move selection down                 |
| `PgUp` / `PgDn`| page up / down                      |
| `g` / `G`      | home / end                          |
| `a`            | toggle Actions on/off               |
| `c`            | toggle Console on/off               |
| `e`            | toggle Errors on/off                |
| `s`            | toggle Stdio on/off                 |
| `E`            | preset: errors only                 |
| `R`            | preset: show all kinds              |
| `r`            | reload trace from disk              |
| `q` / `Esc`    | quit                                |

## Use it as an mnml pane

`mnml-test-playwright` speaks the `tmnl-protocol` blit-host shape
when launched with `--blit <socket>`. mnml can host it inside a
regular pane:

```
:host.launch mnml-test-playwright path/to/trace.zip
```

Mnml core's `tests.open_trace` (`t` key on a failed test row) is
already wired to launch this sibling with the failing test's
retained `trace.zip` — no config needed.

## Status

v0.1 — Trace viewer only. The test runner (`tests.run` /
`tests.run_file` / `tests.run_cursor` / etc.) and the flaky-tests
history viewer stay in mnml core because they need editor
integration (run on the buffer you have open, jump to the source
file on accept). v0.2 may grow the sibling to read mnml's
`.mnml/test-history.json` for a workspace-local flaky list, but
that's predicated on a real use case for "I want this without
running mnml."

## Rename note

This binary was originally published as `mnml-playwright`; renamed
to `mnml-test-playwright` on 2026-06-06 to match the family's
`mnml-<class>-<name>` convention (`mnml-forge-*`, `mnml-db-*`,
`mnml-tracker-*`, etc.). GitHub auto-redirects the old URL. The
old `mnml-playwright` cargo install command still works against the
new repo URL but the binary is named `mnml-test-playwright`.
