mod app;
mod auth;
mod clipboard;
mod config;
mod gitlab;
mod headless;
mod install;
mod keys;
mod theme;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mnml-forge-gitlab", version, about = "GitLab viewer for mnml")]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Headless: print every open MR the configured MR tabs would
    /// surface, as JSON on stdout, then exit. Used by mnml's
    /// `pr.picker` cross-host palette command.
    #[arg(long)]
    list_prs: bool,
    /// Headless: print the URL of the most recent pipeline on
    /// `--branch` in `--owner/--repo`, as `{"url": "..."}` JSON.
    #[arg(long)]
    find_pipeline_for_pr: bool,
    #[arg(long)]
    owner: Option<String>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    branch: Option<String>,
    /// Required for `--list-prs` / `--find-pipeline-for-pr`.
    #[arg(long)]
    json: bool,
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

    let token = auth::load_token()
        .with_context(|| format!("couldn't load token from {}", auth::token_path().display()))?;
    let cfg = config::load()?;
    let client = gitlab::Client::new(&cfg.base_url, &token)?;

    if cli.list_prs {
        if !cli.json {
            anyhow::bail!("--list-prs requires --json (only shape supported v1)");
        }
        return headless::list_prs(&cfg, &client).await;
    }

    if cli.find_pipeline_for_pr {
        if !cli.json {
            anyhow::bail!("--find-pipeline-for-pr requires --json");
        }
        let owner = cli
            .owner
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--owner is required"))?;
        let repo = cli
            .repo
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--repo is required"))?;
        let branch = cli
            .branch
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--branch is required"))?;
        return headless::find_pipeline_for_pr(&client, owner, repo, branch).await;
    }

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!(
            "token: {} (loaded, {} chars)",
            auth::token_path().display(),
            token.len()
        );
        println!("base_url: {}", cfg.base_url);
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        for (i, t) in cfg.tabs.iter().enumerate() {
            println!(
                "  tab {} ({}): kind={} project={:?} mode={:?} state={} ref_name={:?}",
                i + 1,
                t.name,
                t.kind,
                t.project,
                t.mode,
                t.state,
                t.ref_name
            );
        }
        return Ok(());
    }

    if cli.diag {
        println!("mnml-forge-gitlab · diagnostics");
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

    let mut app = app::App::new(cfg, client).await?;

    ui::run(&mut app).await
}
