//! Upload worker + progress parsing.
//!
//! Spawns `aws s3 cp <local> s3://<bucket>/<key>` and parses stderr for
//! progress lines of the shape:
//!
//!   Completed 512.0 KiB/2.7 MiB (5.1 MiB/s) with 1 file(s) remaining
//!
//! The AWS CLI already does multipart uploads automatically for files
//! larger than the multipart threshold (default 8 MiB) — we don't need
//! an SDK for the 5 GiB → 5 TiB story. We just observe what it prints.
//!
//! When piped, the CLI writes progress on stderr terminated by `\r`
//! (carriage return) so the last completion line is a full snapshot;
//! we read byte-by-byte and treat either `\r` or `\n` as a line
//! delimiter.

use anyhow::anyhow;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Instant;

/// One tick of upload progress delivered by the worker thread to the
/// UI. Progress ticks are emitted at most a few times per second; the
/// final tick before `Completed` is a full snapshot so the UI can
/// draw a filled bar even if it dropped intermediate ticks.
#[derive(Debug, Clone)]
pub enum UploadEvent {
    /// bytes done / bytes total. Rate is in bytes/second (0 when we
    /// can't parse it — first tick usually).
    Progress {
        done: u64,
        total: u64,
        rate_bps: u64,
    },
    /// Successful upload — the aws process exited 0.
    Completed,
    /// Upload failed — the aws process exited non-zero, or we
    /// couldn't spawn it.
    Failed(String),
}

/// Spawn one upload in a background thread. Returns a receiver the
/// caller drains from the UI loop.
///
/// `total_bytes` is stat'd upfront so we can render a bar before the
/// first progress line lands (aws is slow to print the first tick on
/// small files).
pub fn spawn_upload(
    local: PathBuf,
    bucket: String,
    key: String,
    region: Option<String>,
    total_bytes: u64,
) -> std::sync::mpsc::Receiver<UploadEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        run_upload(tx, &local, &bucket, &key, region.as_deref(), total_bytes);
    });
    rx
}

fn run_upload(
    tx: Sender<UploadEvent>,
    local: &Path,
    bucket: &str,
    key: &str,
    region: Option<&str>,
    total_bytes: u64,
) {
    let uri = format!("s3://{bucket}/{key}");
    let local_s = local.to_string_lossy().to_string();
    let mut cmd = Command::new("aws");
    if let Some(r) = region {
        cmd.arg("--region").arg(r);
    }
    cmd.arg("s3")
        .arg("cp")
        .arg(&local_s)
        .arg(&uri)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(UploadEvent::Failed(format!(
                "spawn aws: {e} — is the AWS CLI on PATH?"
            )));
            return;
        }
    };

    // First tick — bar is 0% but "known total" so the UI can render.
    let _ = tx.send(UploadEvent::Progress {
        done: 0,
        total: total_bytes,
        rate_bps: 0,
    });

    // Progress goes to stdout with '\r' between updates (CLI v2). We
    // spawn a thread per pipe so a slow reader on one pipe can't stall
    // the other.
    let started = Instant::now();
    if let Some(stdout) = child.stdout.take() {
        let tx_stdout = tx.clone();
        thread::spawn(move || pump_progress(stdout, tx_stdout, total_bytes, started));
    }
    // Stderr — capture for the error message on failure; also parse it
    // (some CLI builds emit progress on stderr).
    let stderr_buf = if let Some(stderr) = child.stderr.take() {
        let tx_stderr = tx.clone();
        let (buf_tx, buf_rx) = channel::<String>();
        thread::spawn(move || {
            let text = pump_progress(stderr, tx_stderr, total_bytes, started);
            let _ = buf_tx.send(text);
        });
        Some(buf_rx)
    } else {
        None
    };

    match child.wait() {
        Ok(status) if status.success() => {
            let _ = tx.send(UploadEvent::Completed);
        }
        Ok(status) => {
            let msg = stderr_buf
                .as_ref()
                .and_then(|rx| rx.recv().ok())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("aws s3 cp exited {status}"));
            let _ = tx.send(UploadEvent::Failed(msg.lines().last().unwrap_or("").into()));
        }
        Err(e) => {
            let _ = tx.send(UploadEvent::Failed(anyhow!("wait aws: {e}").to_string()));
        }
    }
}

/// Read a stream byte-by-byte, split on `\r` or `\n`, parse each chunk
/// as a possible progress line, forward parsed ticks via `tx`. Returns
/// the accumulated non-progress text (used for the failure message).
fn pump_progress<R: Read>(
    stream: R,
    tx: Sender<UploadEvent>,
    total_hint: u64,
    started: Instant,
) -> String {
    let mut reader = BufReader::new(stream);
    let mut accum = String::new();
    let mut line = Vec::<u8>::new();
    let mut byte = [0u8; 1];
    loop {
        // Read one byte at a time so we notice `\r` splits.
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let b = byte[0];
        if b == b'\r' || b == b'\n' {
            if !line.is_empty() {
                let s = String::from_utf8_lossy(&line).to_string();
                if let Some(ev) = parse_progress(&s, total_hint, started.elapsed().as_secs_f64()) {
                    let _ = tx.send(ev);
                } else {
                    accum.push_str(&s);
                    accum.push('\n');
                }
                line.clear();
            }
        } else {
            line.push(b);
        }
    }
    if !line.is_empty() {
        let s = String::from_utf8_lossy(&line).to_string();
        if parse_progress(&s, total_hint, started.elapsed().as_secs_f64()).is_none() {
            accum.push_str(&s);
        }
    }
    // Also consume any remaining full-line text.
    let mut leftover = String::new();
    let _ = reader.read_to_string(&mut leftover);
    if !leftover.is_empty() {
        // Split on newlines and try progress-parse each; anything not
        // a progress line joins accum.
        for chunk in leftover.split(['\r', '\n']) {
            if chunk.is_empty() {
                continue;
            }
            if parse_progress(chunk, total_hint, started.elapsed().as_secs_f64()).is_none() {
                accum.push_str(chunk);
                accum.push('\n');
            }
        }
    }
    accum
}

/// Parse a `Completed X/Y (R)` line into a Progress event. Returns
/// None when the line doesn't match — the caller collects those as
/// failure-message context.
fn parse_progress(line: &str, total_hint: u64, elapsed_secs: f64) -> Option<UploadEvent> {
    // Trim any leading spaces / progress-cursor artifacts.
    let s = line.trim();
    let rest = s.strip_prefix("Completed ")?;
    // Split "512.0 KiB/2.7 MiB (5.1 MiB/s) with 1 file(s) remaining"
    // into ("512.0 KiB/2.7 MiB", "(5.1 MiB/s) …") on the first ' ('.
    let (sizes, tail) = rest.split_once(" (").unwrap_or((rest, ""));
    let (done_s, total_s) = sizes.split_once('/')?;
    let done = parse_size(done_s.trim())?;
    let total = parse_size(total_s.trim()).unwrap_or(total_hint);
    // Rate: everything before " with " or before the trailing ')'.
    let rate_bps = tail
        .split_once(')')
        .map(|(rate, _)| rate.trim().trim_end_matches("/s"))
        .and_then(parse_size_rate)
        .unwrap_or_else(|| {
            if elapsed_secs > 0.0 && done > 0 {
                (done as f64 / elapsed_secs) as u64
            } else {
                0
            }
        });
    Some(UploadEvent::Progress {
        done,
        total,
        rate_bps,
    })
}

/// Parse "512.0 KiB" / "2.7 MiB" / "1024 Bytes" → bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let mut parts = s.split_whitespace();
    let num_s = parts.next()?;
    let unit_s = parts.next().unwrap_or("B");
    let num: f64 = num_s.parse().ok()?;
    let mult: f64 = match unit_s {
        "B" | "Byte" | "Bytes" | "byte" | "bytes" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0f64.powi(4),
        "KB" | "kB" => 1000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        _ => return None,
    };
    Some((num * mult) as u64)
}

fn parse_size_rate(s: &str) -> Option<u64> {
    // "5.1 MiB" (the "/s" has been trimmed by caller). Same units as
    // parse_size — reuse it.
    parse_size(s)
}

/// Format bytes/second as "5.1 MiB/s" — the same unit family the CLI
/// uses so the rendered rate matches what the user sees when they run
/// `aws s3 cp` manually.
pub fn fmt_rate(bps: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bps == 0 {
        return "—".into();
    }
    if bps >= GIB {
        format!("{:.1} GiB/s", bps as f64 / GIB as f64)
    } else if bps >= MIB {
        format!("{:.1} MiB/s", bps as f64 / MIB as f64)
    } else if bps >= KIB {
        format!("{:.1} KiB/s", bps as f64 / KIB as f64)
    } else {
        format!("{bps} B/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kib_line() {
        let ev = parse_progress(
            "Completed 512.0 KiB/2.7 MiB (5.1 MiB/s) with 1 file(s) remaining",
            0,
            0.0,
        )
        .unwrap();
        match ev {
            UploadEvent::Progress {
                done,
                total,
                rate_bps,
            } => {
                assert_eq!(done, 524_288);
                assert_eq!(total, (2.7 * 1024.0 * 1024.0) as u64);
                assert_eq!(rate_bps, (5.1 * 1024.0 * 1024.0) as u64);
            }
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn parses_bytes_line_without_rate() {
        let ev = parse_progress("Completed 42 Bytes/100 Bytes", 100, 1.0).unwrap();
        match ev {
            UploadEvent::Progress {
                done,
                total,
                rate_bps,
            } => {
                assert_eq!(done, 42);
                assert_eq!(total, 100);
                // No rate parsed — fall back to done/elapsed.
                assert_eq!(rate_bps, 42);
            }
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn parses_mib_line_with_bytes_unit() {
        // Some CLI builds print unit "Bytes" not "B" at low sizes.
        let ev = parse_progress(
            "Completed 200 Bytes/2048 Bytes (0 Bytes/s) with 1 file(s) remaining",
            0,
            0.0,
        )
        .unwrap();
        match ev {
            UploadEvent::Progress { done, total, .. } => {
                assert_eq!(done, 200);
                assert_eq!(total, 2048);
            }
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn skips_non_progress_lines() {
        assert!(parse_progress("upload: ./foo.txt to s3://bar/baz", 0, 0.0).is_none());
        assert!(parse_progress("", 0, 0.0).is_none());
        assert!(parse_progress("something", 0, 0.0).is_none());
    }

    #[test]
    fn parse_size_handles_variants() {
        assert_eq!(parse_size("100 B"), Some(100));
        assert_eq!(parse_size("100 Bytes"), Some(100));
        assert_eq!(parse_size("1 KiB"), Some(1024));
        assert_eq!(parse_size("1.5 MiB"), Some((1.5 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_size("2 GiB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("2 GB"), Some(2_000_000_000));
        assert_eq!(parse_size("garbage"), None);
    }

    #[test]
    fn fmt_rate_scales() {
        assert_eq!(fmt_rate(0), "—");
        assert_eq!(fmt_rate(512), "512 B/s");
        assert_eq!(fmt_rate(2048), "2.0 KiB/s");
        assert_eq!(fmt_rate(5 * 1024 * 1024), "5.0 MiB/s");
    }
}
