//! mnml-tracker-jira — terminal TUI for browsing Jira tickets, with
//! configurable per-tab JQL queries and (optionally) auto-resolved
//! release fixVersions.
//!
//! Runs standalone (ratatui + crossterm) by default. With
//! `--blit <socket>` it connects to a tmnl-protocol server (mnml's
//! `pane_host` or tmnl itself) and ships diff'd cell frames over the
//! UDS instead of writing to stdout. The data layer + drawing code
//! are identical between the two modes.

mod app;
mod auth;
mod bitbucket;
mod config;
mod dispatch;
mod install;
mod jira;
mod keys;
mod theme;
mod tree;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mnml-tracker-jira", version, about)]
struct Cli {
    /// Path to the config file. Defaults to
    /// `~/.config/mnml-tracker-jira.toml`.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Print the resolved config + auth setup hints and exit.
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
    /// 2026-07-25 — filter the interactive TUI to a single family
    /// of tabs and hide the tab strip. Powers mnml's three split
    /// "Jira Work" / "Jira Fix Versions" / "Jira Boards" rail
    /// chips (each drops the user straight into a single-purpose
    /// view). Values:
    ///   work         → work_assigned + work_recently_done
    ///   fix-versions → fix_version_tree
    ///   boards       → board_active_sprint + board_backlog
    /// Legacy no-kind tabs are dropped by any --only value —
    /// mixing new and legacy tabs while using --only is not
    /// supported; either migrate the config or omit --only.
    #[arg(long)]
    only: Option<String>,

    /// mnml 0.2.11+ statusline-segment poller invokes this. Fetches
    /// `assignee = currentUser()` + non-Done filter and prints a
    /// JSON payload matching `[[statusline_segments]] format =
    /// "{assigned_open}"`. See #1014. Runs headless: no TUI, no
    /// stdin/stdout drawing — must exit quickly (< poller ceiling).
    #[arg(long)]
    values: bool,
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

    let cfg_path = cli
        .config
        .clone()
        .unwrap_or_else(config::default_config_path);

    let mut cfg = config::load_or_init(&cfg_path)?;

    // 2026-07-25 — --only <family> filters cfg.tabs down to a
    // single split-chip family. Runs BEFORE App::new so the
    // startup pre-fetch loop only touches tabs the user will see.
    let force_hide_strip = if let Some(kind_str) = cli.only.as_deref() {
        let family = config::TabFamily::from_cli(kind_str).ok_or_else(|| {
            anyhow::anyhow!(
                "--only {kind_str:?} unrecognized (want `work` | `fix-versions` | `boards`)"
            )
        })?;
        let before = cfg.tabs.len();
        cfg.tabs
            .retain(|t| t.kind.map(|k| k.family()) == Some(family));
        if cfg.tabs.is_empty() {
            anyhow::bail!(
                "--only {kind_str}: no tabs of that family in {} (had {before} tabs total; \
                 add `kind = \"work_assigned\"` / `\"fix_version_tree\"` / `\"board_active_sprint\"` \
                 etc. to at least one [[tabs]] entry, or omit --only)",
                cfg_path.display()
            );
        }
        true
    } else {
        false
    };

    if cli.check {
        config::print_check_report(&cfg, &cfg_path)?;
        return Ok(());
    }

    let token = auth::load_token()?;
    let client = jira::Client::new(&cfg.jira_url, &cfg.email, &token)?;

    if cli.values {
        // Wrap the fetch in a 10s timeout matching mnml's poller
        // ceiling. If Jira is slow, we'd rather emit nothing than
        // block the poller's worker thread — mnml's chip renders as
        // dim/`!` on empty stdout, which is a legit "still fetching"
        // signal to the user.
        return match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            list_values(&cfg, &client),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => Err(anyhow::anyhow!("--values timed out after 10s")),
        };
    }

    let mut app = app::App::new(cfg, client).await?;
    app.hide_tab_strip = force_hide_strip;
    ui::run(&mut app).await?;
    Ok(())
}

/// Emit `{"assigned_open": N}` on stdout for the statusline chip.
/// Uses the SAME JQL as `TabKind::WorkAssigned` so the chip count
/// matches what the Jira Work Assigned tab displays. Task #1014.
async fn list_values(cfg: &config::Config, client: &jira::Client) -> Result<()> {
    let jql = config::TabKind::WorkAssigned
        .default_jql()
        .ok_or_else(|| anyhow::anyhow!("WorkAssigned kind has no default_jql"))?;
    // No extra fields needed — we only count. Cap at MAX_PAGINATION_ISSUES
    // (500) is fine; any user with >500 assigned open tickets has bigger
    // problems than a wrong chip.
    let issues = client
        .search(jql, 100, &[])
        .await
        .map_err(|e| anyhow::anyhow!("chip fetch failed: {e}"))?;
    let out = serde_json::json!({ "assigned_open": issues.len() });
    println!("{out}");
    Ok(())
}
