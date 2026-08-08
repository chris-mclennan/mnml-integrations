//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/azure_blob.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "azure_blob";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Azure Blob Storage".into(),
        description: Some("Azure Blob storage browser".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-fs-azure-blob".into(),
        category: Some("fs".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F0805}".into(),
            fallback: "Az".into(),
            color: "blue".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "azure_blob.open".into(),
            title: "Azure Blob: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iA".into()],
            run: ":term mnml-fs-azure-blob".into(),
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
