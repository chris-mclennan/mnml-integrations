//! `--install` / `--uninstall` subcommand — writes integration
//! manifests at `~/.config/mnml/integrations/<id>.toml` so mnml
//! picks up the rail chips + palette commands + chord bindings on
//! next startup.
//!
//! 2026-07-22 — split the single `slack` chip into TWO family
//! chips (mirroring the Bitbucket PRs / Pipelines split):
//!
//!   - `slack_channels`   — `mnml-msg-slack --only channels`
//!   - `slack_boards`     — `mnml-msg-slack --only canvases`
//!
//! 2026-08-09 (0.1.2) — three fixes driven by mnml's sibling glyph
//! stability audit (`scratchpad/sibling-audit-2026-08-09.md`) +
//! user morning direction:
//!   * Glyph swap: F117F (channels) + F0F6 (canvases) → F07D2
//!     mdi-slack on BOTH. F117F was accidentally routing to a random
//!     Symbols Nerd Font Mono glyph; F0F6 sat OUTSIDE ghostty's
//!     `font-codepoint-map` ranges entirely → guaranteed tofu.
//!
//! 2026-08-09 (0.1.3) — F07D2 was ALSO wrong: on current Nerd Font
//! versions that codepoint renders as an unrelated (house-shaped)
//! icon, not slack. Bumped to F03EF (also in the routed
//! F0001-F1AFF range) which is the codepoint mnml's own
//! `src/icon_catalog.rs` uses for slack and which renders as the
//! Slack logo on the Nerd Font builds the audit tested.
//!   * Colors: channels → `white`, boards → `yellow`. Distinguishes
//!     the two chips when the glyph is identical.
//!   * `slack_canvases` → `slack_boards` (id + label). Added to the
//!     PREDECESSOR_IDS uninstall cleanup so existing users get the
//!     old `slack_canvases.toml` manifest removed on next `--install`,
//!     ending up with just the new `slack_boards.toml`.
//!
//! `PREDECESSOR_IDS` uninstalls run BEFORE the new manifest writes.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const CHANNELS_ID: &str = "slack_channels";
const BOARDS_ID: &str = "slack_boards";

/// Ids of PRIOR manifest names this sibling has written but no
/// longer wants. Every entry is unconditionally uninstalled on each
/// `--install` so an upgrade doesn't leave orphan chips in the rail.
///
/// - `slack` (pre-0.1) — the single-chip form before the
///   channels/canvases split (2026-07-22).
/// - `slack_canvases` (pre-0.1.2) — renamed to `slack_boards` on
///   2026-08-09 to match Slack's own product-marketing name.
const PREDECESSOR_IDS: &[&str] = &["slack", "slack_canvases"];

pub fn install() -> Result<()> {
    // Drop every legacy manifest first so we don't end up with
    // 3+ chips after upgrading.
    for pid in PREDECESSOR_IDS {
        let _ = uninstall_integration(pid);
    }

    let channels = IntegrationSpec {
        id: CHANNELS_ID.into(),
        label: "Slack Channels".into(),
        description: Some("Slack channels + DMs + threads + search + post".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-slack".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            // F03EF = slack in mnml's `src/icon_catalog.rs`.
            // Ghostty's font-codepoint-map routes F0001-F1AFF to
            // `Symbols Nerd Font Mono`, so this codepoint renders
            // as the canonical Slack logo. (v0.1.2 shipped F07D2
            // on the assumption it was mdi-slack; on current
            // Nerd Font versions F07D2 renders as an unrelated
            // house-shaped glyph.)
            glyph: "\u{F03EF}".into(),
            fallback: "Sk".into(),
            color: "white".into(),
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

    let boards = IntegrationSpec {
        id: BOARDS_ID.into(),
        label: "Slack Boards".into(),
        description: Some("Slack Boards (v0.1 stub — files.list?type=canvas)".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-slack".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            // Same F03EF slack outline as channels — the CHIP
            // COLOR is what tells the two apart in the rail. Sharing
            // the outline matches the "one bundled crate → many
            // chips, one shared glyph, per-chip color" pattern the
            // stability audit proposes.
            glyph: "\u{F03EF}".into(),
            fallback: "SB".into(),
            color: "yellow".into(),
            // 2026-07-22 — enabled by default so users see BOTH
            // chips after --install; the sibling's boards view is
            // still a v0.1 stub but the chip visibility is the
            // affordance we want ("hey, Boards exists — it's
            // coming"). Users can right-click → Remove to hide.
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(BOARDS_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "slack.open_boards".into(),
            title: "Slack: open boards".into(),
            group: Some("integrations".into()),
            keys: vec![],
            // Argument name stays `canvases` — that's the Slack
            // API surface name. Only the mnml-facing chip label
            // and manifest id changed.
            run: ":term mnml-msg-slack --only canvases".into(),
        }],
        ..Default::default()
    };
    let path = install_integration(&boards)?;
    println!("wrote manifest: {}", path.display());

    println!("run mnml + `integrations.refresh` (or restart) to pick up the rail chips");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut removed_any = false;
    for id in PREDECESSOR_IDS.iter().chain([&CHANNELS_ID, &BOARDS_ID]) {
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
