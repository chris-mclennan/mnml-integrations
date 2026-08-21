mod app;
mod clipboard;
mod config;
mod docker;
mod install;
mod keys;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-virt-docker",
    version,
    about = "Docker container/image/volume/network/compose browser for mnml"
)]
struct Cli {
    /// Print the resolved config + daemon state and exit.
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
    /// #1103 f/u7 (2026-08-20) — dump a diagnostic report to stdout
    /// (auth source, config summary, runtime info) and exit.
    #[arg(long)]
    diag: bool,
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

    let cfg = config::load()?;

    if cli.check {
        println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        println!("config: {}", config::config_path().display());
        for (i, t) in cfg.tabs.iter().enumerate() {
            println!(
                "  tab {} ({}): kind={} project_path={:?}",
                i + 1,
                t.name,
                t.kind,
                t.project_path
            );
        }
        let state = docker::probe_daemon();
        match state {
            docker::DaemonState::Ok(v) => {
                println!("daemon: ok · docker server {v}");
            }
            docker::DaemonState::Offline => {
                println!("daemon: offline (start Docker Desktop, then re-run)");
            }
            docker::DaemonState::CliMissing(e) => {
                println!("daemon: docker CLI not found ({e})");
            }
            docker::DaemonState::Error(e) => {
                println!("daemon: error ({e})");
            }
        }
        println!("(auth: defers to the docker socket — no credentials)");
        return Ok(());
    }

    if cli.diag {
        println!("mnml-virt-docker · diagnostics");
        println!();
        println!("Auth");
        println!("  \u{2514}\u{2500} (run `--check` for full auth resolution details)");
        println!();
        println!("Config");
        println!("  \u{2514}\u{2500} path: {}", config::config_path().display());
        println!();
        println!("Runtime");
        println!("  \u{251c}\u{2500} integration: {}", env!("CARGO_PKG_VERSION"));
        println!(
            "  \u{2514}\u{2500} os/arch: {} / {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return Ok(());
    }

    let mut app = app::App::new(cfg)?;
    ui::run(&mut app).await
}
