//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/cloudwatch_logs.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.
//!
//! 2026-08-01 — Stage 2 of the mnml-bridge 0.4 sibling-icons SDK.
//! Instead of relying on mnml core baking `assets/glyphs/aws/cloudwatch.svg`
//! into MnmlSymbols.ttf at a codepoint mnml chose, we now ship our
//! own SVG in-repo (`assets/icons/cloudwatch.svg`) and declare it via
//! `ChipSpec::glyph_svg`. `install_integration` copies the SVG to
//! `~/.config/mnml/glyphs/cloudwatch_logs.svg`; mnml discovers it on
//! next startup + on the `integrations.refresh` palette command, and
//! bakes it into the runtime font on `integrations.bake_sibling_glyphs`.
//!
//! `glyph_codepoint = "F1B09"` pins the SVG at the same codepoint
//! mnml core used to bake it at (see `src/glyph_builder.rs`'s
//! `BUILTIN_GLYPHS` in mnml pre-Stage-2), so users who already have
//! MnmlSymbols.ttf on their system don't see the cloudwatch chip
//! change position when they upgrade.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "cloudwatch_logs";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "CloudWatch Logs".into(),
        description: Some("AWS CloudWatch Logs".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-aws-cloudwatch-logs".into(),
        category: Some("aws".into()),
        chip: Some(ChipSpec {
            // Empty glyph → mnml fills it from the assigned
            // codepoint once the SVG is discovered.
            glyph: String::new(),
            fallback: "CW".into(),
            color: "yellow".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            // 2026-08-01 — mnml-bridge 0.4 sibling-icons SDK.
            // Pin to the codepoint mnml core used to bake
            // cloudwatch at, so upgrading users don't see the chip
            // move.
            glyph_svg_bytes: None,
            glyph_codepoint: Some("F1B09".into()),
        }),
        commands: vec![CommandSpec {
            id: "cloudwatch_logs.open".into(),
            title: "CloudWatch Logs: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iw".into()],
            run: ":term mnml-aws-cloudwatch-logs".into(),
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
