mod app;
mod auth;
mod bridge_client;
mod clipboard;
mod config;
mod github;
mod headless;
mod install;
mod keys;
mod theme;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-forge-github",
    version,
    about = "GitHub Issues viewer for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Headless: print every open PR the configured Issues tabs would
    /// surface, as JSON on stdout, then exit. Used by mnml's
    /// `pr.picker` cross-host palette command and the rail "Open
    /// PRs" subsection refresh.
    #[arg(long)]
    list_prs: bool,
    /// Headless: print the URL of the most recent Actions workflow
    /// run on `--branch` in `--owner/--repo`, as `{"url": "..."}`
    /// JSON on stdout. Used by mnml's pr.picker Tab cross-nav.
    #[arg(long)]
    find_pipeline_for_pr: bool,
    /// Owner for `--find-pipeline-for-pr`.
    #[arg(long)]
    owner: Option<String>,
    /// Repo for `--find-pipeline-for-pr`.
    #[arg(long)]
    repo: Option<String>,
    /// Source branch for `--find-pipeline-for-pr`.
    #[arg(long)]
    branch: Option<String>,
    /// Required for `--list-prs` / `--find-pipeline-for-pr`. Reserves
    /// the headless surface for future shapes.
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

    let token = auth::load_token().context("couldn't load GitHub token")?;
    let cfg = config::load()?;
    let client = github::Client::new(&token)?;

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
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        for (i, t) in cfg.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "actions" => println!(
                    "  tab {} ({}): actions repo={:?} branch={:?}",
                    i + 1,
                    t.name,
                    t.repo,
                    t.branch
                ),
                _ => println!("  tab {} ({}): issues query={:?}", i + 1, t.name, t.query),
            }
        }
        return Ok(());
    }

    let tab_count = cfg.tabs.len();
    let mut app = app::App::new(cfg, client).await?;

    if bridge_client::is_hosted() {
        bridge_client::toast(&format!("mnml-forge-github connected · {tab_count} tab(s)"));
    }

    ui::run(&mut app).await
}
