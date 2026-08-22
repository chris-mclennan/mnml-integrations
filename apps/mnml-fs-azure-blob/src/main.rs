mod app;
mod azure_blob;
mod clipboard;
mod config;
mod install;
mod keys;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-fs-azure-blob",
    version,
    about = "Azure Blob Storage browser for mnml — list, drill, download, yank"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
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
        println!("config: {}", config::config_path().display());
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        for (i, t) in cfg.tabs.iter().enumerate() {
            let kind = match t.kind {
                config::TabKind::Accounts => "accounts".to_string(),
                config::TabKind::Containers => format!(
                    "containers · account={}",
                    t.account.as_deref().unwrap_or("?")
                ),
                config::TabKind::Blobs => format!(
                    "blobs · account={} · container={} · prefix={}",
                    t.account.as_deref().unwrap_or("?"),
                    t.container.as_deref().unwrap_or("?"),
                    t.prefix.as_deref().unwrap_or("")
                ),
            };
            println!("  tab {} ({}): {}", i + 1, t.name, kind);
        }
        // Sanity-check that the Azure CLI is on PATH.
        match std::process::Command::new("az").arg("--version").output() {
            Ok(out) if out.status.success() => {
                let v = String::from_utf8_lossy(&out.stdout);
                // `az --version` is multi-line; show the first line only.
                let first = v.lines().next().unwrap_or("").trim();
                println!("az CLI: ok — {first}");
            }
            Ok(_) => println!("az CLI: FAIL — `az --version` exited non-zero"),
            Err(e) => println!("az CLI: NOT FOUND — {e}"),
        }
        // Check that the user has done `az login`.
        match std::process::Command::new("az")
            .args(["account", "show", "--output", "json"])
            .output()
        {
            Ok(out) if out.status.success() => {
                println!("az auth: ok — `az account show` succeeded");
            }
            Ok(out) => {
                println!(
                    "az auth: NOT LOGGED IN — run `az login` first ({})",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => println!("az auth: FAIL — {e}"),
        }
        return Ok(());
    }

    if cli.diag {
        println!("mnml-fs-azure-blob · diagnostics");
        println!();
        println!("Auth");
        println!("  \u{2514}\u{2500} (run `--check` for full auth resolution details)");
        println!();
        println!("Config");
        println!(
            "  \u{2514}\u{2500} path: {}",
            config::config_path().display()
        );
        println!();
        println!("Runtime");
        println!(
            "  \u{251c}\u{2500} integration: {}",
            env!("CARGO_PKG_VERSION")
        );
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
