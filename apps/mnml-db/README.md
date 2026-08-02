# mnml-db

Unified terminal database viewer for [mnml](https://mnml.sh) — one
integration, per-engine drivers. Phase 1 ships **Postgres** and
**Redis** in one binary; the other five engines (MariaDB,
ClickHouse, Redshift, DocDB, DynamoDB) land in follow-up phases via
new driver crates that plug in through the same `Driver` trait.

Replaces the older per-engine siblings (`mnml-db-postgres`,
`mnml-db-redis`) with a single shell — one connection switcher, one
schema browser, one editor, one integration manifest.

```
┌─ mnml-db ──────────────────────────────────────────────────────────┐
│  Local Postgres  [postgres]  PostgreSQL 16.2 (localhost:5432/postgres)
│  Ctrl+K switch conn · Ctrl+H history · Ctrl+P objects · Ctrl+Enter run
├─────────────┬──────────────────────────────────────────────────────┤
│ ▼ public    │ query [postgres] · Ctrl+Enter run · Ctrl+U clear     │
│   users     │ SELECT id, email FROM users ORDER BY id DESC LIMIT 10│
│   orders    │                                                      │
│ ▶ audit     ├──────────────────────────────────────────────────────┤
│             │ results (10 · 23ms)                                  │
│             │ id      │ email                                      │
│             │ 1234567 │ alice@example.com                          │
│             │ ...                                                  │
└─────────────┴──────────────────────────────────────────────────────┘
 EDITOR   10 rows · 23ms
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-db mnml-db
mnml-db --install     # registers the rail chip in mnml
```

## Setup

1. **Run once** to scaffold the config:

   ```sh
   mnml-db
   ```

   Writes `~/.config/mnml-db/connections.toml` and exits with a
   pointer. `chmod 600` the file to keep the connection metadata
   private.

2. **Edit `connections.toml`.** No plaintext passwords — v1 rejects
   them on load. Reference passwords via an env var:

   ```toml
   row_limit = 500

   [[connection]]
   id = "local-pg"
   label = "Local Postgres"
   engine = "postgres"
   host = "localhost"
   port = 5432
   user = "postgres"
   database = "postgres"
   [connection.creds]
   type = "env"
   password = "PGPASSWORD"

   [[connection]]
   id = "local-redis"
   label = "Local Redis"
   engine = "redis"
   host = "localhost"
   port = 6379
   database = "0"
   ```

   Or via the macOS keychain:

   ```toml
   [connection.creds]
   type = "keychain"
   service = "mnml-db-staging-pg"
   account = "api_readonly"
   ```

3. **Re-run** — the TUI launches on the first connection. Ctrl+K
   swaps between them.

4. **Verify** the resolved config:

   ```sh
   mnml-db --check
   ```

## Keys

| Chord                | Action                                            |
|----------------------|---------------------------------------------------|
| `Ctrl+Enter` / `F5`  | Run the current statement (semicolon-delimited)   |
| `Ctrl+Shift+Enter`   | Run the whole editor                              |
| `Ctrl+K`             | Connection switcher                               |
| `Ctrl+H`             | History picker                                    |
| `Ctrl+P`             | Schema-object picker (inserts `schema.name`)      |
| `Ctrl+Space`         | Trigger completion                                |
| `Tab` / `Shift+Tab`  | Cycle focus: schema → editor → results            |
| `Alt+1`-`Alt+9`      | Jump directly to that connection                  |
| `Ctrl+U`             | Clear the editor                                  |
| `Up` / `Down` / `PgUp` / `PgDn` | Navigate the focused pane              |
| `/` in results       | Start a live-filter                               |
| `R` (uppercase)      | Double `row_limit` for the next run               |
| `Esc`                | Close overlay / clear result filter               |
| `Ctrl+C`             | Quit                                              |

## Adding a new engine

Two files:

1. Author a `mnml-db-driver-<engine>` crate under `drivers/<engine>/`
   that exposes concrete connect / execute / list-namespaces /
   list-objects / describe-object / complete methods.
2. Add a `src/drivers/<engine>.rs` adapter that maps those concrete
   types onto the neutral `Driver` trait in `src/driver.rs`.

Plus one Cargo.toml line — a feature flag and an optional dep on
the driver crate.

## Safety: read-only by convention

`mnml-db` doesn't restrict what you can type — Postgres runs any
SQL, Redis runs any command. Point production connections at a
read-only role / ACL user. The intended use is exploration and
debugging.

## Status & roadmap

**Phase 1 (this release):**

- Postgres driver: SQL query execution, schema tree
  (namespaces → tables/views), NULL rendering, row-limit + truncation,
  keyword completion.
- Redis driver: quote-aware command tokenizer, key/value results,
  per-key TYPE + TTL + peek in the schema tree, Redis command
  completion.
- Shell: connection switcher (Ctrl+K), per-connection MRU history
  (Ctrl+H), schema-object picker (Ctrl+P), live result filter,
  worker-thread driver pattern (query never blocks the render tick).
- `--install` writes a single `db` integration manifest that covers
  every compiled-in engine.

**Phase 2+ (planned):**

- MariaDB driver.
- ClickHouse driver.
- Redshift driver (over Postgres wire protocol, DBM-aware).
- DocDB / DynamoDB drivers (populate the currently-stubbed
  Document result kind).
- Column-width auto-fit + jsonb pretty-print + rich cell rendering.
- Query record / replay.
- IAM auth for RDS + Redshift via AWS profile.

## License

MIT.
