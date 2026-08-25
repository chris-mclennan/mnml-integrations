//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/amplify.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.
//!
//! 2026-08-16 — chip defaults fixed. Two changes:
//!   * `glyph_codepoint`: `F1C0E` → `F1C0E`. F1C0E was colliding
//!     with something else in users' local MnmlSymbols.ttf bakes
//!     (the amplify chip rendered as an unrelated icon). F1C0E is
//!     the codepoint mnml-core's marketplace `catalog_lookup` uses
//!     for `mnml-aws-amplify` — moving here so the Installed chip
//!     matches what the Marketplace tab preview shows.
//!   * `color`: `purple` → `red` (#DD344C — the Security/Front-End
//!     AWS brand color that mnml-core's marketplace catalog
//!     assigns to Amplify).
//!
//! `glyph_svg_bytes` still ships the sibling's own `amplify.svg` —
//! per mnml-core Stage 2 (2026-08-01 in `src/glyph_builder.rs`),
//! the built-in font (`BUILTIN_GLYPHS`) carries no AWS entries;
//! each `mnml-aws-*` sibling owns its SVG and mnml bakes it locally
//! at the pinned codepoint via `integrations.bake_integration_glyphs`.
//! Users on a fresh install without a prior bake at F1C0E rely on
//! the sibling shipping the bytes — dropping them would tofu the
//! chip on every fresh install.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "amplify";
const AMPLIFY_SVG: &[u8] = include_bytes!("../assets/icons/amplify.svg");

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Amplify Deployments".into(),
        description: Some("AWS Amplify app + deploy viewer".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-aws-amplify".into(),
        category: Some("aws".into()),
        chip: Some(ChipSpec {
            // Empty glyph → mnml fills it from the codepoint below
            // via merge_integration_manifests's three-tier resolver.
            glyph: String::new(),
            fallback: "Am".into(),
            color: "red".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            // Bytes embedded in the binary; mnml-bridge writes them
            // to `~/.cache/mnml/pending-glyphs/` for mnml to bake
            // into the user's local MnmlSymbols.ttf at F1C0E on
            // next startup, then deletes the pending file.
            glyph_svg_bytes: Some(AMPLIFY_SVG.to_vec()),
            glyph_codepoint: Some("F1C0E".into()),
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
