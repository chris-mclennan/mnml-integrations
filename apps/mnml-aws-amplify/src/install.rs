//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/amplify.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.
//!
//! 2026-07-31 — this sibling is the first-mover for the mnml-bridge
//! 0.4 sibling-icons SDK. Instead of picking a Nerd Font glyph out
//! of the mnml-core-baked block, we ship our own SVG under
//! `assets/icons/amplify.svg` and declare it via `ChipSpec::glyph_svg`.
//! `install_integration` copies the SVG to
//! `~/.config/mnml/glyphs/amplify.svg`; mnml discovers it on next
//! startup + on the `integrations.refresh` palette command, and
//! bakes it into the runtime font on `integrations.bake_sibling_glyphs`.
//!
//! `glyph_codepoint = "F1B00"` pins the SVG at the same codepoint
//! mnml core used to bake it at (see `src/icon_catalog.rs` in mnml),
//! so users who already have MnmlSymbols.ttf on their system don't
//! see the amplify chip change position when they upgrade.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};
use std::path::PathBuf;

const INTEGRATION_ID: &str = "amplify";

/// Resolve `assets/icons/amplify.svg` to an absolute path. Looks
/// next to the running binary first (release layout), then walks
/// upward for the `assets/` dir (dev / cargo-install layout).
/// Returns `None` if the SVG can't be found — in that case
/// `install_integration` still writes the manifest, mnml just
/// won't have an SVG to bake.
fn amplify_svg_path() -> Option<PathBuf> {
    // 1. Next to the running binary (typical `mnml-aws-amplify --install`
    //    invocation on a released build).
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let cand = dir.join("assets/icons/amplify.svg");
        if cand.exists() {
            return Some(cand);
        }
        // Walk ancestor dirs looking for a `assets/icons/amplify.svg`
        // sibling — cargo-run's target/debug layout.
        let mut cur = dir.to_path_buf();
        while cur.pop() {
            let cand = cur.join("assets/icons/amplify.svg");
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    // 2. CWD — user might run `mnml-aws-amplify --install` from the
    //    repo root during dev.
    let cwd = std::env::current_dir().ok()?;
    let cand = cwd.join("assets/icons/amplify.svg");
    if cand.exists() {
        return Some(cand);
    }
    None
}

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
            // 2026-07-31 — mnml-bridge 0.4 sibling-icons SDK.
            glyph_svg: amplify_svg_path(),
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
