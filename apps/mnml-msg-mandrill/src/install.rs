//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/mandrill.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "mandrill";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Gmail — inbox + sent + labels + search + compose".into(),
        description: Some("Mailchimp Transactional (Mandrill) send + logs".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-mandrill".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F01EF}".into(),
            fallback: "Gm".into(),
            color: "red".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "mandrill.open".into(),
            title: "Mandrill: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iM".into()],
            run: ":term mnml-msg-mandrill".into(),
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
