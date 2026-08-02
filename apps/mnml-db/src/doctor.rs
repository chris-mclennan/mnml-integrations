//! `--doctor` — probe every configured connection with a 1-second
//! connect + describe, print a status table. Meant for the "does
//! anything actually work?" first-run sanity check.
//!
//! Each row: `<id>  <engine>  <status>  <detail>` where status is
//! one of `OK` / `SLOW` (past 1s but succeeded) / `FAIL` (error).

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::Config;
use crate::drivers;

const PER_CONNECTION_TIMEOUT: Duration = Duration::from_millis(1000);

pub async fn run(cfg: &Config) -> Result<()> {
    if cfg.connections.is_empty() {
        println!("no connections configured — edit ~/.config/mnml-db/connections.toml");
        return Ok(());
    }
    // Column widths inferred from the longest id / engine strings so
    // wide labels don't wreck alignment.
    let id_w = cfg
        .connections
        .iter()
        .map(|c| c.id.chars().count())
        .max()
        .unwrap_or(2)
        .max(2);
    let engine_w = cfg
        .connections
        .iter()
        .map(|c| c.engine.chars().count())
        .max()
        .unwrap_or(6)
        .max(6);

    println!(
        "{:<id_w$}  {:<engine_w$}  {:<6}  {}",
        "ID",
        "ENGINE",
        "STATUS",
        "DETAIL",
        id_w = id_w,
        engine_w = engine_w,
    );
    println!(
        "{:<id_w$}  {:<engine_w$}  {:<6}  {}",
        "─".repeat(id_w),
        "─".repeat(engine_w),
        "──────",
        "────────────────────────────────────",
        id_w = id_w,
        engine_w = engine_w,
    );

    for spec in &cfg.connections {
        let start = Instant::now();
        let probe = tokio::time::timeout(PER_CONNECTION_TIMEOUT, drivers::connect(spec)).await;
        let elapsed = start.elapsed();
        let (status, detail) = match probe {
            Err(_) => (
                "FAIL".to_string(),
                format!("timed out after {}ms", PER_CONNECTION_TIMEOUT.as_millis()),
            ),
            // tester 2026-07-31 SEV-1 — `{e:#}` walks the anyhow
            // context chain so the root cause (e.g. "Connection
            // refused") reaches the terminal, not just the outermost
            // frame ("connecting to Postgres").
            Ok(Err(e)) => ("FAIL".to_string(), first_line(&format!("{e:#}"))),
            Ok(Ok(driver)) => {
                let d = driver.describe();
                let label = if elapsed > Duration::from_millis(600) {
                    "SLOW"
                } else {
                    "OK"
                };
                (
                    label.to_string(),
                    format!("{d} · {}ms", elapsed.as_millis()),
                )
            }
        };
        println!(
            "{:<id_w$}  {:<engine_w$}  {:<6}  {}",
            spec.id,
            spec.engine,
            status,
            detail,
            id_w = id_w,
            engine_w = engine_w,
        );
    }
    Ok(())
}

/// Squash multi-line error messages to the first non-empty line so
/// the table stays scannable. Long single-line errors still print in
/// full — no truncation, users can pipe through `less` if they need.
fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(s)
        .trim()
        .to_string()
}
