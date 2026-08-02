//! Minimal OS clipboard helper — shells out to `pbcopy` on
//! macOS, `xclip` / `xsel` / `wl-copy` on Linux. Best-effort;
//! silently no-ops when no known clipboard tool is on PATH.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn yank(text: &str) {
    let (bin, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if which("wl-copy") {
        ("wl-copy", &[])
    } else if which("xclip") {
        ("xclip", &["-selection", "clipboard"])
    } else if which("xsel") {
        ("xsel", &["--clipboard", "--input"])
    } else {
        return;
    };
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.stdin(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
}

fn which(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in path.split(':') {
        if std::path::Path::new(dir).join(bin).is_file() {
            return true;
        }
    }
    false
}
