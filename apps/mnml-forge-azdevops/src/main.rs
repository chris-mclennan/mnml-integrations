mod app;
mod auth;
mod azdevops;
mod clipboard;
mod config;
mod headless;
mod install;
mod keys;
mod theme;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-forge-azdevops",
    version,
    about = "Azure DevOps viewer for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Headless: print every open PR the configured PR tabs would
    /// surface, as JSON on stdout.
    #[arg(long)]
    list_prs: bool,
    /// Headless: print the URL of the most recent build for
    /// `--branch` in `--owner/--repo`, where owner is
    /// `<org>/<project>`. JSON shape: `{"url": "..."}`.
    #[arg(long)]
    find_pipeline_for_pr: bool,
    /// `<org>/<project>` for `--find-pipeline-for-pr`.
    #[arg(long)]
    owner: Option<String>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    branch: Option<String>,
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
    let client = azdevops::Client::new(&token)?;

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
        // owner is "<org>/<project>" for Azure DevOps.
        let (org, project) = owner
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("--owner must be `<org>/<project>` for Azure DevOps"))?;
        return headless::find_pipeline_for_pr(&client, org, project, repo, branch).await;
    }

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!(
            "token: {} (loaded, {} chars)",
            auth::token_path().display(),
            token.len()
        );
        println!("org: {}", cfg.org);
        if let Some(p) = &cfg.project {
            println!("project: {p}");
        }
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        for (i, t) in cfg.tabs.iter().enumerate() {
            println!(
                "  tab {} ({}): kind={} project={:?} repo={:?} mode={:?} state={}",
                i + 1,
                t.name,
                t.kind,
                t.project,
                t.repo,
                t.mode,
                t.state
            );
        }
        return Ok(());
    }

    let mut app = app::App::new(cfg, client).await?;

    ui::run(&mut app).await
}
