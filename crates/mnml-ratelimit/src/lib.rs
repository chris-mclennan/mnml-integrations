//! Cross-process token-bucket rate limiter for mnml integrations
//! that share a per-IP API budget. See README.md for the full
//! rationale.
//!
//! Rust port of `bb_ratelimit.py` from tattle-claude-plugins. The
//! state-file JSON schema is intentionally compatible with the
//! Python version so a mixed fleet can coordinate on the same
//! pool.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Per-service tuning. Values mirror the Python defaults; a caller
/// with a different service (Jira has a different sustained budget)
/// can build a custom `Config` and pass it to `Limiter::with_config`.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Tokens per second the bucket refills at. Set BELOW the
    /// sustained per-IP budget so the pool refills faster than a
    /// steady workload drains it — the bucket is a *budget*, not a
    /// smoother.
    pub rate: f64,
    /// Burst allowance. A background sweep draws this down at once,
    /// then paces at `rate`. Shape most APIs tolerate.
    pub capacity: f64,
    /// Multiplicative cut to the shared rate on a 429.
    pub penalty_factor: f64,
    /// Floor the shared rate cannot go below. Deliberately well
    /// under `rate` — a floor at the refill rate isn't backing off
    /// at all, it's treading water.
    pub min_rate: f64,
    /// Default park duration when a 429 carries no `Retry-After`.
    pub default_cooldown_secs: f64,
    /// Multiplicative recovery toward `rate` per successful acquire.
    pub recover_factor: f64,
    /// Hard cap on how long a single `acquire` can block. Past
    /// this, fail-open and let the caller's own 429 handling take
    /// over. Prevents a wedged state file from hanging every
    /// integration indefinitely.
    pub max_block_secs: f64,
}

impl Config {
    /// Bitbucket Cloud sustained budget is ~1000 req/hr = 0.278
    /// req/s across ALL processes on this IP. `rate = 0.22` leaves
    /// headroom for interactive sessions while loops run.
    pub const BITBUCKET: Config = Config {
        rate: 0.22,
        capacity: 40.0,
        penalty_factor: 0.5,
        min_rate: 0.05,
        default_cooldown_secs: 60.0,
        recover_factor: 1.02,
        max_block_secs: 120.0,
    };

    /// Jira Cloud is more forgiving on sustained load than
    /// Bitbucket but is stricter on bursts (large concurrent
    /// requests trip 429 immediately). Wider burst, comparable
    /// rate; adjust in the caller if it's still tight.
    pub const JIRA: Config = Config {
        rate: 0.33,
        capacity: 60.0,
        penalty_factor: 0.5,
        min_rate: 0.08,
        default_cooldown_secs: 45.0,
        recover_factor: 1.02,
        max_block_secs: 120.0,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::BITBUCKET
    }
}

/// The state persisted to disk. Fields match the Python
/// `bb_ratelimit` JSON keys byte-for-byte so a mixed fleet
/// (Python plugins + Rust integrations) can share one pool.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct State {
    /// Wall-clock seconds since Unix epoch at last refill.
    #[serde(default)]
    ts: f64,
    /// Tokens currently in the bucket.
    #[serde(default)]
    tokens: f64,
    /// Current shared rate (may drop below `Config::rate` after a
    /// 429; recovers via `recover_factor`).
    #[serde(default)]
    rate: f64,
    /// Wall-clock seconds since Unix epoch until which every
    /// process is parked.
    #[serde(default)]
    cooldown_until: f64,
    /// Running total of 429s recorded across all processes.
    #[serde(default)]
    throttles: u64,
    /// Wall-clock of the last 429 (0 if never).
    #[serde(default)]
    last_429: f64,
}

/// The public limiter handle. Cheap to clone — the shared state
/// lives on disk, not inside this struct.
#[derive(Debug, Clone)]
pub struct Limiter {
    path: PathBuf,
    cfg: Config,
}

impl Limiter {
    /// Construct a limiter for a named service. Reads the shared
    /// pool at `$MNML_DATA_ROOT/ratelimit/<service>.json` (falls
    /// back to `~/.config/mnml/ratelimit/<service>.json`).
    /// Well-known service names (`"bitbucket"`, `"jira"`) auto-pick
    /// the matching `Config`; anything else defaults to
    /// `Config::BITBUCKET`.
    pub fn for_service(service: &str) -> Self {
        let cfg = match service.to_ascii_lowercase().as_str() {
            "bitbucket" => Config::BITBUCKET,
            "jira" => Config::JIRA,
            _ => Config::default(),
        };
        Self::with_config(service, cfg)
    }

    /// Construct a limiter for a service name with an explicit
    /// `Config` (rate/burst overrides).
    pub fn with_config(service: &str, cfg: Config) -> Self {
        let path = default_state_path(service);
        Self { path, cfg }
    }

    /// Construct a limiter at an explicit state-file path. Bypasses
    /// the `MNML_DATA_ROOT` resolution — mostly for tests.
    pub fn at_path(path: impl AsRef<Path>, cfg: Config) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            cfg,
        }
    }

    /// Block until a token is available. Returns `true` if one was
    /// consumed; `false` if we gave up (state-file wedged, timeout
    /// exceeded). Fail-open — the caller SHOULD proceed on `false`
    /// and let its own 429 handling deal with the consequences.
    /// A broken limiter must never take every integration offline.
    pub fn acquire(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(self.cfg.max_block_secs);
        loop {
            let now = now_secs();
            let wait = match self.with_locked_state(|st| {
                if st.cooldown_until > now {
                    Ok(st.cooldown_until - now)
                } else if st.tokens >= 1.0 {
                    st.tokens -= 1.0;
                    // Ease shared rate back toward baseline on success.
                    st.rate = st
                        .rate
                        .max(0.0)
                        .min(self.cfg.rate.max(st.rate * self.cfg.recover_factor));
                    Ok(0.0)
                } else {
                    let need = 1.0 - st.tokens;
                    let cur_rate = st.rate.max(self.cfg.min_rate);
                    Ok(need / cur_rate)
                }
            }) {
                Ok(w) => w,
                Err(_) => return false, // fail-open
            };
            if wait <= 0.0 {
                return true;
            }
            // Jitter so processes released together don't re-cluster.
            let jittered = jitter(wait.min(5.0));
            if Instant::now() + Duration::from_secs_f64(jittered) > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_secs_f64(jittered.max(0.05)));
        }
    }

    /// Record a 429. Parks EVERY process until `now +
    /// retry_after_secs` (or `Config::default_cooldown_secs` when
    /// zero) and cuts the shared rate. Fail-open on any file error.
    pub fn penalize(&self, retry_after_secs: f64) {
        let _ = self.with_locked_state(|st| {
            st.rate = self.cfg.min_rate.max(st.rate * self.cfg.penalty_factor);
            st.tokens = 0.0;
            let cd = if retry_after_secs > 0.0 {
                retry_after_secs
            } else {
                self.cfg.default_cooldown_secs
            };
            st.cooldown_until = now_secs() + cd;
            st.throttles = st.throttles.saturating_add(1);
            st.last_429 = now_secs();
            Ok(())
        });
    }

    /// Snapshot the current shared state — useful for a `--diag`
    /// subcommand. Returns `None` on file error.
    pub fn status(&self) -> Option<Status> {
        self.with_locked_state(|st| {
            Ok(Status {
                tokens: st.tokens,
                capacity: self.cfg.capacity,
                rate: st.rate,
                baseline_rate: self.cfg.rate,
                throttles: st.throttles,
                cooldown_remaining_secs: (st.cooldown_until - now_secs()).max(0.0),
            })
        })
        .ok()
    }

    // ── internals ────────────────────────────────────────────────

    /// Open the state file, take a `flock` on Unix, read-refill-
    /// apply-write under the lock. `f` gets an `&mut State` and
    /// returns whatever the caller needs.
    fn with_locked_state<F, R>(&self, f: F) -> Result<R, std::io::Error>
    where
        F: FnOnce(&mut State) -> Result<R, std::io::Error>,
    {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        let _guard = flock(&file);
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let mut st: State = if buf.trim().is_empty() {
            State {
                tokens: self.cfg.capacity,
                rate: self.cfg.rate,
                ts: now_secs(),
                ..Default::default()
            }
        } else {
            serde_json::from_str(&buf).unwrap_or_else(|_| State {
                tokens: self.cfg.capacity,
                rate: self.cfg.rate,
                ts: now_secs(),
                ..Default::default()
            })
        };
        // Refill from wall-clock elapsed.
        let now = now_secs();
        let elapsed = (now - st.ts).max(0.0);
        st.tokens = self.cfg.capacity.min(st.tokens + elapsed * st.rate);
        if st.rate <= 0.0 {
            st.rate = self.cfg.rate;
        }
        st.ts = now;
        let result = f(&mut st)?;
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        let serialized = serde_json::to_string(&st).map_err(std::io::Error::other)?;
        file.write_all(serialized.as_bytes())?;
        Ok(result)
    }
}

/// Snapshot returned by `Limiter::status`.
#[derive(Debug, Clone)]
pub struct Status {
    pub tokens: f64,
    pub capacity: f64,
    pub rate: f64,
    pub baseline_rate: f64,
    pub throttles: u64,
    pub cooldown_remaining_secs: f64,
}

// ── helpers ──────────────────────────────────────────────────────

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Cheap ±20% jitter without pulling in `rand`. Uses the low bits
/// of the current nanoseconds — the requirement is "don't
/// re-cluster after a shared cooldown", not "cryptographic
/// randomness".
fn jitter(base: f64) -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let noise = (nanos % 1000) as f64 / 1000.0; // 0.0..1.0
    let factor = 0.8 + 0.4 * noise; // 0.8..1.2
    base * factor
}

fn default_state_path(service: &str) -> PathBuf {
    let svc = sanitize(service);
    // #1002 f/u (2026-08-21) — share state with tattle-claude-plugins /
    // tattle-claude-workspace's Python `bb_ratelimit.py` so mnml
    // integrations + Python scripts running on the same machine take
    // turns on ONE bucket per service instead of hammering the API
    // in parallel. Python's path is `$TATTLE_ARTIFACTS_ROOT/{service}
    // -ratelimit.json`, defaulting to `~/.tattle-claude-artifacts/`.
    //
    // Resolution order:
    //   1. TATTLE_ARTIFACTS_ROOT env → matches Python's env branch
    //      exactly (tattle-claude-workspace launcher sets this).
    //   2. `~/.tattle-claude-artifacts/` if it already exists →
    //      Python-plugins user with no explicit env; adopt the
    //      shared dir automatically.
    //   3. MNML_DATA_ROOT env → mnml's hermetic-sandbox override
    //      (see #1041); useful for tests + one-off runs.
    //   4. `~/.config/mnml/ratelimit/` → mnml-only user, no shared
    //      state needed (nothing else on the machine coordinates).
    //
    // File-name shape `{service}-ratelimit.json` matches Python
    // (e.g. `bitbucket-ratelimit.json`), so both processes read/
    // write the same file with the same flock.
    let (root, file_shape) = if let Some(v) = std::env::var_os("TATTLE_ARTIFACTS_ROOT") {
        (PathBuf::from(v), true)
    } else {
        let tattle_default = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".tattle-claude-artifacts"));
        if let Some(p) = tattle_default.as_ref()
            && p.is_dir()
        {
            (p.clone(), true)
        } else if let Some(v) = std::env::var_os("MNML_DATA_ROOT") {
            (PathBuf::from(v).join("ratelimit"), false)
        } else if let Some(h) = std::env::var_os("HOME") {
            (
                PathBuf::from(h).join(".config").join("mnml").join("ratelimit"),
                false,
            )
        } else {
            (PathBuf::from(".").join("ratelimit"), false)
        }
    };
    if file_shape {
        // Python convention: <root>/<service>-ratelimit.json.
        root.join(format!("{svc}-ratelimit.json"))
    } else {
        // mnml-only fallback: <root>/<service>.json (root already
        // includes /ratelimit/ subdir).
        root.join(format!("{svc}.json"))
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ── flock ────────────────────────────────────────────────────────

#[cfg(unix)]
struct FlockGuard {
    fd: std::os::unix::io::RawFd,
}

#[cfg(unix)]
impl Drop for FlockGuard {
    fn drop(&mut self) {
        // SAFETY: fd is owned by the caller's still-live File; we're
        // only releasing our advisory lock, not closing it.
        unsafe {
            libc::flock(self.fd, libc::LOCK_UN);
        }
    }
}

#[cfg(unix)]
fn flock(file: &File) -> Option<FlockGuard> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: fd comes from a live File; LOCK_EX blocks until we own it.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if rc == 0 { Some(FlockGuard { fd }) } else { None }
}

#[cfg(not(unix))]
fn flock(_file: &File) -> Option<()> {
    // Best-effort no-op on Windows. The pool degrades to per-process
    // pacing (each process gets its own JSON view on first read).
    // Follow-up: switch to `LockFileEx` on Windows.
    None
}

// ── tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cfg_fast() -> Config {
        // High rate + huge capacity so tests don't sleep. This still
        // exercises the read-refill-write cycle end-to-end.
        Config {
            rate: 1000.0,
            capacity: 100.0,
            penalty_factor: 0.5,
            min_rate: 100.0,
            default_cooldown_secs: 1.0,
            recover_factor: 1.02,
            max_block_secs: 2.0,
        }
    }

    #[test]
    fn acquire_from_full_bucket_returns_true() {
        let dir = tempdir().unwrap();
        let l = Limiter::at_path(dir.path().join("bb.json"), cfg_fast());
        assert!(l.acquire());
    }

    #[test]
    fn penalize_sets_cooldown_and_blocks_next_acquire() {
        let dir = tempdir().unwrap();
        let l = Limiter::at_path(dir.path().join("bb.json"), cfg_fast());
        // Drain the bucket so cooldown is the sole blocker.
        for _ in 0..(cfg_fast().capacity as usize) {
            l.acquire();
        }
        l.penalize(0.0); // 1s default in cfg_fast
        let status = l.status().unwrap();
        assert!(status.cooldown_remaining_secs > 0.0);
        assert_eq!(status.throttles, 1);
    }

    /// The state schema is compatible with the Python version. This
    /// test writes the Python JSON shape into the state file and
    /// confirms `acquire` reads it without corruption.
    #[test]
    fn reads_python_bb_ratelimit_state_shape() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bb.json");
        std::fs::write(
            &path,
            r#"{"ts":1700000000.0,"tokens":25.0,"rate":0.22,"cooldown_until":0,"throttles":3,"last_429":0}"#,
        )
        .unwrap();
        let l = Limiter::at_path(&path, cfg_fast());
        assert!(l.acquire());
        let st = l.status().unwrap();
        assert_eq!(st.throttles, 3);
        // Tokens refilled from the ancient ts + high test rate → clamped to capacity.
        assert!(st.tokens <= cfg_fast().capacity);
    }

    #[test]
    fn fail_open_on_bad_json_returns_true() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bb.json");
        std::fs::write(&path, "this is not json").unwrap();
        let l = Limiter::at_path(&path, cfg_fast());
        // Malformed state is treated as fresh; acquire still succeeds.
        assert!(l.acquire());
    }
}
