//! `mnml-db` — the unified database viewer / query playground for
//! mnml. One binary, per-engine drivers (Postgres + Redis in v0.1).
//!
//! Modes:
//!   - `--install` / `--uninstall` writes/removes the integration
//!     manifest and exits.
//!   - `--check` prints the resolved config (with redacted DSN
//!     summaries) and exits.
//!   - default → launches the TUI, standalone or hosted-in-mnml
//!     (detected via `MNML_PANE`).

mod app;
mod config;
mod connection;
mod doctor;
mod driver;
mod drivers;
mod history;
mod install;
mod keys;
mod query;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-db",
    version,
    about = "Unified database viewer for mnml (Postgres, Redis, more coming)"
)]
struct Cli {
    /// Print the resolved config + connection summaries and exit.
    #[arg(long)]
    check: bool,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/db.toml. Also
    /// removes the 7 per-engine predecessor manifests (postgres,
    /// redis, mariadb, clickhouse, redshift, docdb, dynamodb) so
    /// the rail chip list consolidates. Pass --keep-predecessors
    /// to skip that cleanup.
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest.
    #[arg(long)]
    uninstall: bool,
    /// With --install, keep the 7 per-engine predecessor
    /// manifests in place instead of removing them. No effect
    /// without --install.
    #[arg(long)]
    keep_predecessors: bool,
    /// Remove the 7 per-engine predecessor manifests without
    /// (re)installing `mnml-db` itself. Idempotent.
    #[arg(long)]
    uninstall_predecessors: bool,
    /// Probe every configured connection with a 1-second connect +
    /// describe, print a status table. Useful for the "does this
    /// actually work" first-run sanity check.
    #[arg(long)]
    doctor: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Install / uninstall run before touching the config so the
    // first-run install doesn't require credentials to be set up.
    if cli.install {
        return install::install(cli.keep_predecessors);
    }
    if cli.uninstall {
        return install::uninstall();
    }
    if cli.uninstall_predecessors {
        install::uninstall_predecessors();
        return Ok(());
    }

    let cfg = config::load()?;

    if cli.doctor {
        return doctor::run(&cfg).await;
    }

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!("row_limit: {}", cfg.row_limit);
        for (i, c) in cfg.connections.iter().enumerate() {
            let host = c.host.as_deref().unwrap_or("-");
            let port = c
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string());
            let db = c.database.as_deref().unwrap_or("-");
            let user = c.user.as_deref().unwrap_or("-");
            println!(
                "  connection {} ({}): engine={} {}@{}:{}/{}",
                i + 1,
                c.display_label(),
                c.engine,
                user,
                host,
                port,
                db
            );
        }
        return Ok(());
    }

    let runtime = tokio::runtime::Handle::current();
    let mut a = app::App::new(cfg, runtime);
    ui::run(&mut a).await
}
