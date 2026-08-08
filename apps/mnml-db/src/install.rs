//! `--install` / `--uninstall` — writes an integration manifest at
//! `~/.config/mnml/integrations/db.toml`. The single manifest
//! covers *all* engines this build supports; the shell then routes
//! between them via the connection switcher.
//!
//! Phase 4 (2026-07-31): `--install` also auto-uninstalls the 7
//! predecessor sibling manifests (postgres, redis, mariadb, …) so
//! the rail chip list consolidates from 7 chips down to 1. Pass
//! `--keep-predecessors` to skip that step.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "db";

/// The per-engine sibling manifests that `mnml-db` supersedes.
/// Kept in one list so the predecessor sweep in `install()` / the
/// explicit `--uninstall-predecessors` flag stay in sync.
const PREDECESSOR_IDS: &[&str] = &[
    "postgres",
    "redis",
    "mariadb",
    "clickhouse",
    "redshift",
    "docdb",
    "dynamodb",
];

pub fn install(keep_predecessors: bool) -> Result<()> {
    if !keep_predecessors {
        uninstall_predecessors();
    }
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Databases".into(),
        description: Some(
            "Database viewer — SQL playground for Postgres, command playground for Redis, more engines coming.".into(),
        ),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-db".into(),
        category: Some("db".into()),
        chip: Some(ChipSpec {
            // nf-oct-database
            glyph: "\u{F01BC}".into(),
            fallback: "D".into(),
            color: "cyan".into(),
            enabled: true,
            in_palette_bar: true,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "db.open".into(),
            title: "DB: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>id".into()],
            run: ":term mnml-db".into(),
        }],
        ..Default::default()
    };
    let path = install_integration(&spec)?;
    println!("wrote manifest: {}", path.display());
    println!("run mnml + `integrations.refresh` (or restart) to pick up the rail chip");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let removed = uninstall_integration(INTEGRATION_ID)?;
    if removed {
        println!("removed manifest for {INTEGRATION_ID}");
    } else {
        println!("no manifest for {INTEGRATION_ID} (already uninstalled)");
    }
    Ok(())
}

/// Remove every per-engine sibling manifest this crate supersedes.
/// Called from `install()` by default; also exposed via the
/// `--uninstall-predecessors` CLI flag so users can run it
/// standalone (e.g. after `--install --keep-predecessors`, then
/// deciding they DO want the consolidation).
pub fn uninstall_predecessors() -> u32 {
    let mut removed = 0u32;
    for id in PREDECESSOR_IDS {
        match uninstall_integration(id) {
            Ok(true) => {
                println!("removed predecessor manifest: {id}");
                removed += 1;
            }
            Ok(false) => {} // wasn't installed — silent
            Err(e) => eprintln!("failed to uninstall {id}: {e}"),
        }
    }
    if removed == 0 {
        println!("no predecessor manifests to remove");
    } else {
        println!("consolidated {removed} predecessor manifest(s) into `db`");
    }
    removed
}
