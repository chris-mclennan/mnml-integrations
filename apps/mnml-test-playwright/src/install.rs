//! `--install` / `--uninstall` subcommand — writes an integration
//! manifest at `~/.config/mnml/integrations/playwright.toml` so mnml
//! picks up the rail chip + palette command + chord binding on
//! next startup.

use anyhow::Result;
use mnml_bridge::{
    ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const INTEGRATION_ID: &str = "playwright";

pub fn install() -> Result<()> {
    let spec = IntegrationSpec {
        id: INTEGRATION_ID.into(),
        label: "Playwright traces".into(),
        description: Some("Playwright trace viewer".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-test-playwright".into(),
        category: Some("test".into()),
        chip: Some(ChipSpec {
            glyph: "\u{F0E66}".into(),
            fallback: "Pw".into(),
            color: "green".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(INTEGRATION_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "playwright.open".into(),
            title: "Playwright: open".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>ip".into()],
            run: ":term mnml-test-playwright".into(),
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
