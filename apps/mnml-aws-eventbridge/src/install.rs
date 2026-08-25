//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/eventbridge.toml` so mnml
//! picks up the rail chip + palette command + chord binding on
//! next startup.
//!
//! 2026-08-04 — updated to mnml-bridge 0.5 `glyph_svg_bytes` API.
//! The SVG is embedded in this binary via `include_bytes!`. Bridge
//! writes bytes to `~/.cache/mnml/pending-glyphs/eventbridge.svg`;
//! mnml bakes at next startup + deletes the pending file — no
//! permanent glyph SVG anywhere under `~/.config/mnml/`.
//!
//! `glyph_codepoint = "F1C09"` pins the SVG at the same codepoint
//! mnml core used to bake it at (see `src/glyph_builder.rs`'s
//! `BUILTIN_GLYPHS` in mnml pre-Stage-2), so users who already have
//! MnmlSymbols.ttf on their system don't see the eventbridge chip
//! change position when they upgrade.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "eventbridge";
const EVENTBRIDGE_SVG: &[u8] = include_bytes!("../assets/icons/eventbridge.svg");

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "EventBridge Schedules".into(),
        description: Some("EventBridge Schedules (time + target JSON editor)".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-aws-eventbridge".into(),
        category: Some("aws".into()),
        chip: Some(ChipSpec {
            // Empty glyph → mnml fills it from the assigned
            // codepoint once the SVG is discovered.
            glyph: String::new(),
            fallback: "EB".into(),
            color: "magenta".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            // 2026-08-01 — mnml-bridge 0.4 sibling-icons SDK.
            glyph_svg_bytes: Some(EVENTBRIDGE_SVG.to_vec()),
            // Pin to the codepoint mnml core used to bake
            // eventbridge at, so upgrading users don't see the chip
            // move.
            glyph_codepoint: Some("F1C09".into()),
        }),
        commands: vec![CommandSpec {
            id: "eventbridge.open".into(),
            title: "EventBridge: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iv".into()],
            run: ":term mnml-aws-eventbridge".into(),
        }],
        ..Default::default()
    };
    let path = install_integration(&spec)?;
    println!("wrote manifest: {}", path.display());
    println!(
        "run mnml + `integrations.refresh` (or restart) to pick up the rail chip; \
         then `integrations.bake_integration_glyphs` to bake the SVG into MnmlSymbols.ttf"
    );
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
