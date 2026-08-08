//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/docker.toml` so mnml
//! picks up the rail chip + palette command + chord binding on
//! next startup. Mirrors the IntegrationIcon default that shipped
//! in mnml core through 0.1.4.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "docker";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Docker".into(),
        description: Some("Docker containers + images + volumes + networks".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-virt-docker".into(),
        category: Some("virt".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F0868}".into(), // nf-md-docker
            fallback: "Dk".into(),
            color: "blue".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "docker.open".into(),
            title: "Docker: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iK".into()],
            run: ":term mnml-virt-docker".into(),
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
