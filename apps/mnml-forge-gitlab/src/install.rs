//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/gitlab.toml`
//! so mnml picks up the rail chip + palette command + chord
//! binding on next startup. Mirrors the IntegrationIcon default
//! that shipped in mnml core through 0.1.4; from 0.2.0 onwards
//! the sibling owns its own registration.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "gitlab";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "GitLab".into(),
        description: Some("GitLab CI + MRs viewer".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-forge-gitlab".into(),
        category: Some("forge".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F296}".into(),
            fallback: "L".into(),
            color: "orange".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "gitlab.open".into(),
            title: "GitLab: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>il".into()],
            run: ":term mnml-forge-gitlab".into(),
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
