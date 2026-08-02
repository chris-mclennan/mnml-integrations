//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/bitbucket.toml` so
//! mnml picks up the rail chip + palette command + chord binding
//! on next startup. Mirrors the IntegrationIcon default that
//! shipped in mnml core through 0.1.4; from 0.2.0 onwards the
//! sibling owns its own registration.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "bitbucket";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Bitbucket pipelines + PRs".into(),
        description: Some("Bitbucket Cloud PR + pipelines viewer".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-forge-bitbucket".into(),
        category: Some("forge".into()),
        chip: Some(ChipSpec {
            glyph: "\u{E703}".into(), // nf-dev-bitbucket
            fallback: "B".into(),
            color: "blue".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some("bitbucket".into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "bitbucket.open".into(),
            title: "Bitbucket: open PR + pipelines viewer".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>ib".into()],
            run: ":term mnml-forge-bitbucket".into(),
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
