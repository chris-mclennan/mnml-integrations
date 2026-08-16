//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/amplify.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.
//!
//! 2026-08-16 — chip defaults reconciled with mnml core's AWS
//! SVG family (F1C03–F1C0E). Prior versions shipped the sibling's
//! own `amplify.svg` and pinned `F1B00`, competing with the mnml-
//! core bake at `F1C0E` for the same integration — the F1B00 slot
//! ended up baked with an inconsistent glyph (users saw it render
//! as an unrelated icon) and the color drifted from AWS brand
//! purple to the family-correct AWS red. The sibling now:
//!   * declares `glyph_codepoint = "F1C0E"` so the chip resolves
//!     to the mnml-core-baked AWS Amplify glyph;
//!   * uses `color = "red"` (#DD344C — the Security/Front-End AWS
//!     brand color that mnml core's marketplace catalog assigns
//!     to this integration);
//!   * no longer ships `glyph_svg_bytes` — mnml-core already owns
//!     the F1C0E bake for the whole `mnml-aws-*` family, so a
//!     second sibling-shipped SVG would just overwrite it.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "amplify";

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
            // mnml-core owns the F1C0E bake for the AWS Amplify
            // glyph (part of the AWS SVG family baked from
            // ~/Downloads/mnml-aws-icon-preview-inverted at
            // F1C03–F1C0E). Sibling only pins the codepoint;
            // no sibling-shipped SVG.
            glyph_svg_bytes: None,
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
