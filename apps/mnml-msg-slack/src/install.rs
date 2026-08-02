//! `--install` / `--uninstall` subcommand — writes integration
//! manifests at `~/.config/mnml/integrations/<id>.toml` so mnml
//! picks up the rail chips + palette commands + chord bindings on
//! next startup.
//!
//! 2026-07-22 — split the single `slack` chip into TWO family
//! chips (mirroring the Bitbucket PRs / Pipelines split):
//!
//!   - `slack_channels`   — `mnml-msg-slack --only channels`
//!   - `slack_canvases`   — `mnml-msg-slack --only canvases`
//!
//! The legacy `slack` id is uninstalled on install so users don't
//! see three chips. Uninstall wipes all three.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const LEGACY_ID: &str = "slack";
const CHANNELS_ID: &str = "slack_channels";
const CANVASES_ID: &str = "slack_canvases";

pub fn install() -> Result<()> {
    // Drop the legacy single-chip manifest if it's still around.
    let _ = uninstall_integration(LEGACY_ID);

    let channels = IntegrationSpec {
        id: CHANNELS_ID.into(),
        label: "Slack Channels".into(),
        description: Some("Slack channels + DMs + threads + search + post".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-slack".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F117F}".into(),
            fallback: "Sk".into(),
            color: "magenta".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(CHANNELS_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "slack.open_channels".into(),
            title: "Slack: open channels".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iS".into()],
            run: ":term mnml-msg-slack --only channels".into(),
        }],
        ..Default::default()
    };
    let path = install_integration(&channels)?;
    println!("wrote manifest: {}", path.display());

    let canvases = IntegrationSpec {
        id: CANVASES_ID.into(),
        label: "Slack Canvases".into(),
        description: Some("Slack Canvases (v0.1 stub — files.list?type=canvas)".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-slack".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F0F6}".into(),
            fallback: "SC".into(),
            color: "cyan".into(),
            // 2026-07-22 — enabled by default so users see BOTH
            // chips after --install; the sibling's canvases view is
            // still a v0.1 stub but the chip visibility is the
            // affordance we want ("hey, Canvases exists — it's
            // coming"). Users can right-click → Remove to hide.
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(CANVASES_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "slack.open_canvases".into(),
            title: "Slack: open canvases".into(),
            group: Some("integrations".into()),
            keys: vec![],
            run: ":term mnml-msg-slack --only canvases".into(),
        }],
        ..Default::default()
    };
    let path = install_integration(&canvases)?;
    println!("wrote manifest: {}", path.display());

    println!("run mnml + `integrations.refresh` (or restart) to pick up the rail chips");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut removed_any = false;
    for id in [LEGACY_ID, CHANNELS_ID, CANVASES_ID] {
        if uninstall_integration(id)? {
            println!("removed manifest for {id}");
            removed_any = true;
        }
    }
    if !removed_any {
        println!("no manifests found (already uninstalled)");
    }
    Ok(())
}
