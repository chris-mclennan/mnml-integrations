//! `--install` / `--uninstall` subcommands.
//!
//! 2026-07-25 — split into three rail chips to match the bitbucket
//! split pattern. Each chip drops the user straight into a
//! single-purpose view via `--only <family>`:
//!   - Jira Work         → `mnml-tracker-jira --only work`
//!   - Jira Fix Versions → `mnml-tracker-jira --only fix-versions`
//!   - Jira Boards       → `mnml-tracker-jira --only boards`
//!
//! The pre-split combined "jira" manifest gets removed on install
//! so it doesn't duplicate as a fourth chip. `--uninstall` removes
//! all four (three splits + the legacy combined) for symmetry with
//! a user who's been on both schemes over time.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const LEGACY_ID: &str = "jira";

/// One rail chip's registration data. `id` doubles as the manifest
/// filename (`~/.config/mnml/integrations/<id>.toml`) and the
/// mnml-side chip id for right-click / config overrides.
struct SplitChip {
    id: &'static str,
    name: &'static str,
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

/// The three split chips. Same glyph (Jira icon, F0411) across
/// all three — the tooltip + fallback letters distinguish them.
/// Colors: work=blue (mine), fix-versions=green (releases),
/// boards=magenta (planning) — matches the roles.
const SPLITS: &[SplitChip] = &[
    SplitChip {
        id: "jira_work",
        name: "Jira Work",
        description: "Jira: tickets assigned to me + recently done",
        fallback: "JW",
        label: "Jira Work — assigned to me",
        color: "blue",
        only_flag: "work",
        leader_keys: "<leader>ijw",
        command_id: "jira_work.open",
        command_title: "Jira Work: open",
    },
    SplitChip {
        id: "jira_fix_versions",
        name: "Jira Fix Versions",
        description: "Jira: current release grouped by status, with linked PRs + pipelines",
        fallback: "JV",
        label: "Jira Fix Versions — release tracker",
        color: "green",
        only_flag: "fix-versions",
        leader_keys: "<leader>ijv",
        command_id: "jira_fix_versions.open",
        command_title: "Jira Fix Versions: open",
    },
    SplitChip {
        id: "jira_boards",
        name: "Jira Boards",
        description: "Jira: active sprint + backlog",
        fallback: "JB",
        label: "Jira Boards — sprint + backlog",
        color: "magenta",
        only_flag: "boards",
        leader_keys: "<leader>ijb",
        command_id: "jira_boards.open",
        command_title: "Jira Boards: open",
    },
];

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
            binary: "mnml-tracker-jira".into(),
            category: Some("tracker".into()),
            chip: Some(ChipSpec {
                // nf-md-jira glyph everywhere — the three chips
                // are one family; the label / fallback letters
                // split them apart.
                glyph: "\u{F0411}".into(),
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
                run: format!(":term mnml-tracker-jira --only {}", chip.only_flag),
            }],
            ..Default::default()
        };
        let path = install_integration(&spec)?;
        println!("wrote manifest: {}", path.display());
    }
    println!("\nrun mnml + `integrations.refresh` (or restart) to pick up the new chips");
    println!("chords: <leader>ijw / ijv / ijb  ·  right-click chip for options");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut any = false;
    // Legacy first (in case the user is only on the pre-split
    // shape), then the three splits.
    for id in [LEGACY_ID].into_iter().chain(SPLITS.iter().map(|c| c.id)) {
        if uninstall_integration(id)? {
            println!("removed manifest for {id}");
            any = true;
        }
    }
    if !any {
        println!("no jira manifests present (already uninstalled)");
    }
    Ok(())
}
