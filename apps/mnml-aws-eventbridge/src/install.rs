//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/eventbridge.toml` so mnml
//! picks up the rail chip + palette command + chord binding on
//! next startup.
//!
//! 2026-08-01 — Stage 2 of the mnml-bridge 0.4 sibling-icons SDK.
//! Instead of relying on mnml core baking `assets/glyphs/aws/eventbridge.svg`
//! into MnmlSymbols.ttf at a codepoint mnml chose, we now ship our
//! own SVG in-repo (`assets/icons/eventbridge.svg`) and declare it via
//! `ChipSpec::glyph_svg`. `install_integration` copies the SVG to
//! `~/.config/mnml/glyphs/eventbridge.svg`; mnml discovers it on next
//! startup + on the `integrations.refresh` palette command, and bakes
//! it into the runtime font on `integrations.bake_sibling_glyphs`.
//!
//! `glyph_codepoint = "F1B0B"` pins the SVG at the same codepoint
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
            color: "pink".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            // 2026-08-01 — mnml-bridge 0.4 sibling-icons SDK.
            glyph_svg: None,
            glyph_svg_bytes: Some(EVENTBRIDGE_SVG.to_vec()),
            // Pin to the codepoint mnml core used to bake
            // eventbridge at, so upgrading users don't see the chip
            // move.
            glyph_codepoint: Some("F1B0B".into()),
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
         then `integrations.bake_sibling_glyphs` to bake the SVG into MnmlSymbols.ttf"
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
