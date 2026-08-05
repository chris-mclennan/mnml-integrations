//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/meta_stats.toml` so mnml
//! picks up the rail chip + palette command on next startup.
//!
//! No glyph SVG bytes shipped — the chip uses the "MS" fallback
//! text glyph and picks up the standard chip color scheme. A
//! prettier vector icon can land in a later patch bump without a
//! bridge version change.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "meta_stats";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "mnml Meta Stats".into(),
        description: Some("Download counts for every mnml-* crate + GitHub release".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-meta-stats".into(),
        category: Some("meta".into()),
        chip: Some(ChipSpec {
            glyph: String::new(),
            fallback: "MS".into(),
            color: "cyan".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "meta_stats.open".into(),
            title: "Meta: crates.io + release download stats".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>im".into()],
            run: ":term mnml-meta-stats".into(),
        }],
        ..Default::default()
    };
    let path = install_integration(&spec)?;
    println!("wrote manifest: {}", path.display());
    println!("run mnml + `integrations.refresh` (or restart) to pick up the rail chip");
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
