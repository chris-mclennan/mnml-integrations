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
    /// #1103 f/u7 (2026-08-20) — dump a human-readable diagnostic
    /// report to stdout, then exit. Covers auth resolution, live
    /// /myself probe, config summary, and version info. Powers
    /// mnml's `integrations.diag` palette command + the "Run
    /// diagnostics" chip context menu.
    #[arg(long)]
    diag: bool,
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
    /// #1117 (2026-08-21) — mnml core's prefetch worker invokes
    /// `mnml-tracker-jira --prefetch --only <kind>` on a background
    /// cadence and stashes stdout under
    /// `~/.cache/mnml/prefetch/jira_work-<id>.json`. When the user
    /// then opens the corresponding pane, mnml passes the cache
    /// path via `MNML_PREFETCH_CACHE_FILE` and the interactive
    /// launch hydrates from it instead of doing a cold Jira fetch.
    ///
    /// Emits a JSON object of shape:
    ///   { "generated_at": <unix_secs>,
    ///     "tabs": [
    ///       { "name": "Assigned", "issues": [ /* Issue */ ] },
    ///       ...
    ///     ] }
    ///
    /// Runs headless — no TUI. Same 10s timeout as `--values` so a
    /// slow Jira doesn't wedge the worker.
    #[arg(long)]
    prefetch: bool,
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

    if cli.prefetch {
        // #1117 (2026-08-21) — background prefetch producer. Build
        // the App the same way an interactive launch would (so all
        // tabs are resolved + `refresh_active` fetches the same
        // ticket list the pane will show), then emit the tabs
        // structure as JSON for mnml's prefetch worker to cache.
        // 10s timeout matches --values so a slow Jira doesn't wedge
        // the worker.
        return match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_prefetch(cfg, client),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => Err(anyhow::anyhow!("--prefetch timed out after 10s")),
        };
    }

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

    if cli.diag {
        return run_diag(&cfg, &cfg_path, &client, token.len()).await;
    }

    let mut app = app::App::new(cfg, client).await?;
    app.hide_tab_strip = force_hide_strip;
    ui::run(&mut app).await?;
    Ok(())
}

/// #1117 (2026-08-21) — background prefetch producer. Constructs
/// the same App the interactive launch would (so tab resolution +
/// initial fetches match exactly), then serializes each tab's
/// issue list as JSON. mnml core caches stdout to
/// `~/.cache/mnml/prefetch/<int>-<id>.json` and stamps the path on
/// the child env via `MNML_PREFETCH_CACHE_FILE` when the pane
/// opens. The interactive launch checks that env in `App::new` +
/// hydrates from JSON instead of doing a cold fetch — the pane
/// paints populated on frame one.
async fn run_prefetch(cfg: config::Config, client: jira::Client) -> Result<()> {
    #[derive(serde::Serialize)]
    struct PrefetchCache {
        generated_at: u64,
        tabs: Vec<PrefetchTab>,
    }
    #[derive(serde::Serialize)]
    struct PrefetchTab {
        name: String,
        jql: String,
        issues: Vec<jira::Issue>,
    }
    // App::new only fetches the active tab. For multi-tab families
    // (a Work integration with Assigned + Recently Worked + Unified),
    // walk every tab and refresh so the cache covers all of them —
    // otherwise the hydrator would populate empty lists for tabs 1+
    // AND mark them `last_fetched`, telling the pane to skip its own
    // refresh, which is strictly worse than not hydrating.
    let mut app = app::App::new(cfg, client).await?;
    for idx in 0..app.tabs.len() {
        if app.tabs[idx].last_fetched.is_none() {
            app.active_tab = idx;
            app.refresh_active().await;
        }
    }
    let tabs: Vec<PrefetchTab> = app
        .tabs
        .iter()
        .map(|t| PrefetchTab {
            name: t.name.clone(),
            jql: t.jql.clone(),
            issues: t.issues.clone(),
        })
        .collect();
    let cache = PrefetchCache {
        generated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        tabs,
    };
    println!("{}", serde_json::to_string(&cache)?);
    Ok(())
}

/// #1103 f/u7 (2026-08-20) — human-readable diagnostic dump.
/// Mirrors the mnml-forge-bitbucket `--diag` shape: Auth (source
/// + live /myself probe), Config, Runtime. Sections run
///   independently so a failure in one still shows the rest.
async fn run_diag(
    cfg: &config::Config,
    cfg_path: &std::path::Path,
    client: &jira::Client,
    token_len: usize,
) -> Result<()> {
    println!("mnml-tracker-jira · diagnostics");
    println!();
    println!("Auth");
    println!("  ├─ token source: {}", auth::token_path().display());
    println!("  ├─ token length: {token_len} chars");
    println!("  ├─ email: {}", cfg.email);
    println!("  ├─ jira_url: {}", cfg.jira_url);
    match client.myself().await {
        Ok(account_id) => {
            println!("  └─ /myself: ✓ account_id={account_id}");
        }
        Err(e) => {
            println!("  └─ /myself: ✗ {e}");
            println!("     JQL queries and statusline chip depend on this succeeding.");
        }
    }
    println!();
    println!("Config");
    println!("  ├─ path: {}", cfg_path.display());
    println!("  ├─ jira_url: {}", cfg.jira_url);
    if cfg.projects.is_empty() {
        println!("  ├─ projects allowlist: (none — spans every visible project)");
    } else {
        println!("  ├─ projects allowlist: {} entries", cfg.projects.len());
        for p in cfg.projects.iter().take(10) {
            println!("  │   {p}");
        }
        if cfg.projects.len() > 10 {
            println!("  │   … and {} more", cfg.projects.len() - 10);
        }
    }
    println!("  └─ tabs: {}", cfg.tabs.len());
    for (i, t) in cfg.tabs.iter().enumerate() {
        let kind = t
            .kind
            .map(|k| format!("{:?}", k))
            .unwrap_or_else(|| "<legacy>".to_string());
        println!("      {}. {} (kind={kind})", i + 1, t.name);
    }
    println!();
    println!("Runtime");
    println!("  ├─ integration: {}", env!("CARGO_PKG_VERSION"));
    println!(
        "  └─ os/arch: {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(())
}

/// Emit `{"assigned_open": N}` on stdout for the statusline chip.
/// Uses the SAME JQL as `TabKind::WorkAssigned` so the chip count
/// matches what the Jira Work Assigned tab displays. Task #1014.
///
/// #1029 (2026-08-18) — when `cfg.projects` is non-empty, scopes
/// the JQL to those project keys. Mirrors the Bitbucket `repos`
/// allowlist idea (#1028) for a Jira instance that spans many
/// projects, most of which aren't yours. Empty preserves old
/// behavior (count across every project the user can see).
async fn list_values(cfg: &config::Config, client: &jira::Client) -> Result<()> {
    let base = config::TabKind::WorkAssigned
        .default_jql()
        .ok_or_else(|| anyhow::anyhow!("WorkAssigned kind has no default_jql"))?;
    let jql: String = if cfg.projects.is_empty() {
        base.to_string()
    } else {
        // Sanitize: JQL project keys are 2-10 uppercase ASCII
        // chars. Anything else silently drops so a bad config
        // can't inject arbitrary JQL. Empty result after filter
        // → fall back to the unscoped query.
        let safe: Vec<&String> = cfg
            .projects
            .iter()
            .filter(|k| {
                let s = k.as_str();
                !s.is_empty()
                    && s.len() <= 10
                    && s.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            })
            .collect();
        if safe.is_empty() {
            base.to_string()
        } else {
            // Prepend `project in (KEY1, KEY2) AND `. Base JQL
            // already starts with `assignee = currentUser() AND …`,
            // so simple prefix + `AND` is safe.
            let list = safe
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("project in ({list}) AND {base}")
        }
    };
    // No extra fields needed — we only count. Cap at MAX_PAGINATION_ISSUES
    // (500) is fine; any user with >500 assigned open tickets has bigger
    // problems than a wrong chip.
    let issues = client
        .search(&jql, 100, &[])
        .await
        .map_err(|e| anyhow::anyhow!("chip fetch failed: {e}"))?;
    let out = serde_json::json!({ "assigned_open": issues.len() });
    println!("{out}");
    Ok(())
}
