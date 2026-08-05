//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/amplify.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.
//!
//! 2026-08-04 — updated to mnml-bridge 0.5 `glyph_svg_bytes` API.
//! The SVG is embedded in this binary via `include_bytes!`, so
//! `--install` never needs to look up the SVG on disk (the old
//! path-based lookup failed on `cargo install` where the assets/
//! dir didn't ship). Bridge writes bytes to
//! `~/.cache/mnml/pending-glyphs/amplify.svg`; mnml bakes at next
//! startup + deletes the pending file — no permanent glyph SVG
//! anywhere under `~/.config/mnml/`.
//!
//! `glyph_codepoint = "F1B00"` pins the SVG at the same codepoint
//! mnml core used to bake it at (see `src/icon_catalog.rs` in mnml),
//! so users who already have MnmlSymbols.ttf on their system don't
//! see the amplify chip change position when they upgrade.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "amplify";
const AMPLIFY_SVG: &[u8] = include_bytes!("../assets/icons/amplify.svg");

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Amplify apps + deploys".into(),
        description: Some("AWS Amplify app + deploy viewer".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-aws-amplify".into(),
        category: Some("aws".into()),
        chip: Some(ChipSpec {
            // Empty glyph → mnml fills it from the assigned
            // codepoint once the SVG is discovered. Leaving it
            // empty (not the old \u{F087D}) means the sibling
            // doesn't have to pick a codepoint from the mnml core
            // PUA layout — the SDK handles that.
            glyph: String::new(),
            fallback: "Am".into(),
            color: "purple".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            // 2026-08-04 — mnml-bridge 0.5. Bytes are embedded in
            // the binary; bridge writes them to
            // `~/.cache/mnml/pending-glyphs/` for mnml to bake +
            // delete on next startup.
            glyph_svg_bytes: Some(AMPLIFY_SVG.to_vec()),
            // Pin to the codepoint mnml core used to bake amplify
            // at, so upgrading users don't see the chip move.
            glyph_codepoint: Some("F1B00".into()),
        }),
        commands: vec![CommandSpec {
            id: "amplify.open".into(),
            title: "Amplify: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>ia".into()],
            run: ":term mnml-aws-amplify".into(),
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
