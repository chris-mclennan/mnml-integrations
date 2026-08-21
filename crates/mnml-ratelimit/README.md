# mnml-ratelimit

Cross-process token-bucket rate limiter for mnml integrations that
share an API budget scoped to a single machine / IP.

## Why

Bitbucket, Jira Cloud, and similar hosts throttle per-IP. Multiple
mnml integrations running on the same machine (statusline chip
worker + the pane fetcher + a right-panel drill-in) will each
politely pace themselves and *still* burn the budget because
politeness has to be shared, not local. This crate stores the
token pool in one file on disk that every process on the machine
draws from under an advisory lock.

Rust port of the Python `bb_ratelimit` used by tattle-claude-plugins.
The state-file format is compatible so a mixed fleet (some plugins
on Python, some on Rust) can share the same pool.

## Usage

```rust
use mnml_ratelimit::Limiter;

let l = Limiter::for_service("bitbucket");   // shared JSON file per service
l.acquire();                                  // block until a token frees
// ... make the request ...
if got_429 {
    l.penalize(retry_after_secs);             // 429 parks EVERY process
}
```

## Design

- Token bucket at `RATE` tokens/sec with `CAPACITY` burst, both
  service-specific (see `Config`). Rate sits under the sustained
  budget so the bucket is a *budget*, not a smoother.
- Shared state file (one JSON blob) protected by `flock(2)` on
  Unix; a best-effort no-op on Windows (each process ends up with
  a local bucket, which degrades to per-process pacing).
- Global cooldown on 429 — one throttle parks every process until
  `cooldown_until`, and cuts the shared rate.
- Fail-open — any error touching the state file returns `true` from
  `acquire` and pretends the call went through, so a broken
  limiter cannot take every integration offline.

## State file

Default path is `$MNML_DATA_ROOT/ratelimit/<service>.json` (falls
back to `~/.config/mnml/ratelimit/<service>.json`). Override with
`Limiter::at_path(&Path)`.
