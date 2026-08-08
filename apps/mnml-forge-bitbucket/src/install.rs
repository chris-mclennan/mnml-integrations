//! `--install` / `--uninstall` subcommands.
//!
//! 2026-08-07 — split into rail chips matching the `mnml-tracker-jira`
//! pattern. Each chip drops the user straight into a single-purpose
//! view via `--only <family>`:
//!   - Bitbucket PRs        → `mnml-forge-bitbucket --only prs`
//!   - Bitbucket Pipelines  → `mnml-forge-bitbucket --only pipelines`
//!
//! 2026-08-08 — Branches chip removed. User report: the branches
//! surface was accidentally shipped by an earlier iteration and
//! doesn't reflect a real supported view. `--uninstall` still tries
//! to remove `bitbucket_branches` so anyone who installed the
//! branches chip while it was there gets it cleaned up.
//!
//! The pre-split combined "bitbucket" manifest gets removed on
//! install so it doesn't duplicate as a third chip. `--uninstall`
//! removes all four (the two current splits + legacy combined +
//! retired branches) for symmetry with a user who's been on any of
//! the historical schemes.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const LEGACY_ID: &str = "bitbucket";

/// One rail chip's registration data. `id` doubles as the manifest
/// filename (`~/.config/mnml/integrations/<id>.toml`) and the
/// mnml-side chip id for right-click / config overrides.
struct SplitChip {
    id: &'static str,
    description: &'static str,
    fallback: &'static str,
    label: &'static str,
    color: &'static str,
    only_flag: &'static str,
    /// Leader chord for the palette command. Kept distinct per
    /// chip so users can wire quick access to any of the three.
    leader_keys: &'static str,
    command_id: &'static str,
    command_title: &'static str,
}

/// The two split chips. Same glyph (Bitbucket icon, E703) across
/// both — the tooltip + fallback letters distinguish them.
/// Colors: prs=blue (review flow), pipelines=green (build health).
const SPLITS: &[SplitChip] = &[
    SplitChip {
        id: "bitbucket_prs",
        description: "Bitbucket: open + merged pull requests across the workspace",
        fallback: "BP",
        label: "Bitbucket PRs",
        color: "blue",
        only_flag: "prs",
        leader_keys: "<leader>ibp",
        command_id: "bitbucket_prs.open",
        command_title: "Bitbucket PRs: open",
    },
    SplitChip {
        id: "bitbucket_pipelines",
        description: "Bitbucket: recent pipeline runs per repo/branch",
        fallback: "BL",
        label: "Bitbucket Pipelines",
        color: "green",
        only_flag: "pipelines",
        leader_keys: "<leader>ibl",
        command_id: "bitbucket_pipelines.open",
        command_title: "Bitbucket Pipelines: open",
    },
];

/// Retired chips — install won't create them, but `--uninstall` will
/// remove them if a user is still on an older install schema.
const RETIRED_IDS: &[&str] = &["bitbucket_branches"];

pub fn install() -> Result<()> {
    // Remove the legacy combined manifest first so a fresh install
    // doesn't leave four chips in the rail. Silent if it's already
    // gone — common case for new users.
    if uninstall_integration(LEGACY_ID)? {
        println!("removed legacy combined manifest ({LEGACY_ID}) — replaced by three split chips");
    }

    for chip in SPLITS {
        let spec = IntegrationSpec {
            id: chip.id.into(),
            label: chip.label.into(),
            description: Some(chip.description.into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            binary: "mnml-forge-bitbucket".into(),
            category: Some("forge".into()),
            chip: Some(ChipSpec {
                // nf-dev-bitbucket glyph everywhere — the three chips
                // are one family; the label / fallback letters split
                // them apart.
                glyph: "\u{F00A8}".into(),
                fallback: chip.fallback.into(),
                color: chip.color.into(),
                enabled: true,
                in_palette_bar: false,
                badge_key: Some(chip.id.into()),
                ..Default::default()
            }),
            commands: vec![CommandSpec {
                id: chip.command_id.into(),
                title: chip.command_title.into(),
                group: Some("integrations".into()),
                keys: vec![chip.leader_keys.into()],
                run: format!(":term mnml-forge-bitbucket --only {}", chip.only_flag),
            }],
            ..Default::default()
        };
        let path = install_integration(&spec)?;
        println!("wrote manifest: {}", path.display());
    }
    println!("\nrun mnml + `integrations.refresh` (or restart) to pick up the new chips");
    println!("chords: <leader>ibp / ibl / ibb  ·  right-click chip for options");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut any = false;
    // Legacy combined first, then the current splits, then any
    // retired split chips (bitbucket_branches, etc.) so a user on
    // ANY historical install shape gets a clean sweep.
    for id in [LEGACY_ID]
        .into_iter()
        .chain(SPLITS.iter().map(|c| c.id))
        .chain(RETIRED_IDS.iter().copied())
    {
        if uninstall_integration(id)? {
            println!("removed manifest for {id}");
            any = true;
        }
    }
    if !any {
        println!("no bitbucket manifests present (already uninstalled)");
    }
    Ok(())
}
