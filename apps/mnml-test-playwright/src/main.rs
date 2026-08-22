mod app;
mod install;
mod keys;
#[allow(dead_code)]
mod trace;
#[allow(dead_code)]
mod trace_pane;
mod ui;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-playwright",
    version,
    about = "Playwright trace.zip viewer for mnml"
)]
struct Cli {
    /// Path to a Playwright `trace.zip` to open.
    #[arg(value_name = "TRACE_ZIP")]
    trace: Option<PathBuf>,
    /// Print version + exit.
    #[arg(long)]
    check: bool,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/<id>.toml so the
    /// rail chip + palette command + chord appear on the next
    /// mnml startup (or after `integrations.refresh`).
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest for this sibling.
    #[arg(long)]
    uninstall: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // --install / --uninstall run before auth / config so the
    // first-run install doesn't require credentials to be set up.
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }

    if cli.check {
        println!("mnml-playwright v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let trace_path = cli
        .trace
        .ok_or_else(|| anyhow!("expected a trace.zip path as the positional argument"))?;
    let mut app = app::App::open(trace_path).context("failed to open trace")?;

    ui::run(&mut app).await
}
