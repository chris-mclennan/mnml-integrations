//! mnml-msg-gcal — terminal Google Calendar client for the mnml
//! family. Browses today / week / upcoming meetings and creates
//! quick events. Uses the Calendar API v3 with loopback OAuth
//! against a per-user GCP project.
//!
//! Runs standalone (`mnml-msg-gcal`) or hosted as an mnml Pty
//! pane (`:term mnml-msg-gcal`, or `<leader>iC` after
//! `mnml-msg-gcal --install`).
//!
//! This is v0.1 — the CLI shape, config layout, `--install`
//! integration, and OAuth loopback are wired; the TUI event loop
//! and Calendar API calls are stubs. See TODO markers in
//! `app.rs` / `gcal.rs` / `ui.rs`.

mod app;
mod auth;
mod clipboard;
mod config;
mod gcal;
mod install;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-gcal",
    version,
    about = "Google Calendar browse + create for mnml"
)]
struct Cli {
    /// Print the resolved config + OAuth setup hints and exit.
    #[arg(long)]
    check: bool,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/gcal.toml so the
    /// rail chip + palette command + <leader>iC chord appear on
    /// the next mnml startup (or after `integrations.refresh`).
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest for this sibling.
    #[arg(long)]
    uninstall: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // --install / --uninstall run before auth so first-run install
    // doesn't need credentials.
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }

    if cli.check {
        return check();
    }

    // TUI event loop (v1 stub — see app.rs for the shape).
    app::run()
}

fn check() -> Result<()> {
    let cfg = config::load()?;
    println!("config: {}", config::config_path().display());
    println!("  calendar_id: {}", cfg.calendar_id);
    println!("  timezone:    {}", cfg.timezone);
    println!("  refresh_secs: {}", cfg.refresh_secs);
    println!();
    match auth::load_token() {
        Ok(_) => println!(
            "oauth: OK — token cached at {}",
            auth::token_path().display()
        ),
        Err(e) => {
            println!("oauth: NOT SET — {}", e);
            println!();
            println!("Setup:");
            println!("  1. Create a per-user Google Cloud project + enable Calendar API v3.");
            println!("  2. Create OAuth 2.0 Client ID (Desktop app).");
            println!("  3. Save client_id + client_secret to");
            println!("     ~/.config/mnml-msg-gcal/client.toml:");
            println!("       client_id     = \"...\"");
            println!("       client_secret = \"...\"");
            println!("  4. Run mnml-msg-gcal → browser opens loopback OAuth flow.");
        }
    }
    Ok(())
}
