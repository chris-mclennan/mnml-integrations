//! Tiny client around mnml's tier-2 JSONL IPC. Cheap — no syscalls
//! until called. `is_hosted()` returns true iff we were spawned by
//! mnml (env vars present).
//!
//! mnml hosts siblings as Pty panes and sets `MNML_IPC_DIR`,
//! `MNML_WORKSPACE`, and `MNML_THEME` on every spawn. Writing a
//! single JSONL line to `<MNML_IPC_DIR>/command` (append) is enough
//! to ask the host to toast / open a file / spawn a sub-pty.
//!
//! Best-effort: every fn silently no-ops when not hosted or when the
//! write fails. Sibling tools must keep working standalone.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// True iff mnml exported `MNML_IPC_DIR` into our environment.
///
/// Cached on first call — safe to invoke from hot paths (render loops,
/// per-event handlers). The env var is set by mnml before our exec and
/// won't change for the lifetime of the process.
pub fn is_hosted() -> bool {
    static HOSTED: OnceLock<bool> = OnceLock::new();
    *HOSTED.get_or_init(|| std::env::var_os("MNML_IPC_DIR").is_some())
}

/// Theme name mnml advertised (e.g. "cyberdream"). None when not hosted.
#[allow(dead_code)]
pub fn theme() -> Option<String> {
    std::env::var("MNML_THEME").ok()
}

/// Workspace path mnml advertised. None when not hosted.
#[allow(dead_code)]
pub fn workspace() -> Option<PathBuf> {
    std::env::var_os("MNML_WORKSPACE").map(PathBuf::from)
}

/// Best-effort toast — silently no-op when not hosted or write fails.
pub fn toast(message: &str) {
    let Ok(line) = serde_json::to_string(&serde_json::json!({
        "cmd": "toast",
        "text": message,
    })) else {
        return;
    };
    write_line(&line);
}

/// Best-effort "open this file in mnml's editor". Silently no-ops when
/// the path isn't valid UTF-8 (would round-trip lossily through JSON
/// and mnml would fail to locate the file with no explanation).
#[allow(dead_code)]
pub fn open_file(path: &Path) {
    let Some(p) = path.to_str() else { return };
    let Ok(line) = serde_json::to_string(&serde_json::json!({
        "cmd": "open",
        "path": p,
    })) else {
        return;
    };
    write_line(&line);
}

/// Append one already-serialized JSONL line to `<MNML_IPC_DIR>/command`.
/// Errors swallowed.
fn write_line(line: &str) {
    let Some(dir) = std::env::var_os("MNML_IPC_DIR") else {
        return;
    };
    let path = PathBuf::from(dir).join("command");
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(f, "{line}");
}
