//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/codebuild.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.
//!
//! 2026-08-01 — Stage 2 of the mnml-bridge 0.4 sibling-icons SDK.
//! Instead of relying on mnml core baking `assets/glyphs/aws/codebuild.svg`
//! into MnmlSymbols.ttf at a codepoint mnml chose, we now ship our
//! own SVG in-repo (`assets/icons/codebuild.svg`) and declare it via
//! `ChipSpec::glyph_svg`. `install_integration` copies the SVG to
//! `~/.config/mnml/glyphs/codebuild.svg`; mnml discovers it on next
//! startup + on the `integrations.refresh` palette command, and bakes
//! it into the runtime font on `integrations.bake_sibling_glyphs`.
//!
//! `glyph_codepoint = "F1B0A"` pins the SVG at the same codepoint
//! mnml core used to bake it at (see `src/glyph_builder.rs`'s
//! `BUILTIN_GLYPHS` in mnml pre-Stage-2), so users who already have
//! MnmlSymbols.ttf on their system don't see the codebuild chip
//! change position when they upgrade.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};
const INTEGRATION_ID: &str = "codebuild";
const CODEBUILD_SVG: &[u8] = include_bytes!("../assets/icons/codebuild.svg");

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "AWS CodeBuild + logs".into(),
        description: Some("AWS CodeBuild project + build viewer".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-aws-codebuild".into(),
        category: Some("aws".into()),
        chip: Some(ChipSpec {
            // Empty glyph → mnml fills it from the assigned
            // codepoint once the SVG is discovered.
            glyph: String::new(),
            fallback: "C".into(),
            color: "yellow".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            // 2026-08-01 — mnml-bridge 0.4 sibling-icons SDK.
            glyph_svg: None,
            glyph_svg_bytes: Some(CODEBUILD_SVG.to_vec()),
            glyph_codepoint: Some("F1B0A".into()),
        }),
        commands: vec![CommandSpec {
            id: "codebuild.open".into(),
            title: "CodeBuild: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>ic".into()],
            run: ":term mnml-aws-codebuild".into(),
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
