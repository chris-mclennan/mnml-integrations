//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/buttondown.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "buttondown";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Buttondown".into(),
        description: Some("Buttondown newsletter draft + send".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-buttondown".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F0EB1}".into(),
            fallback: "Bd".into(),
            color: "green".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "buttondown.open".into(),
            title: "Buttondown: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iB".into()],
            run: ":term mnml-msg-buttondown".into(),
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
