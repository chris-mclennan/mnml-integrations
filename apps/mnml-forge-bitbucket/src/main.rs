mod app;
mod auth;
mod bitbucket;
mod clipboard;
mod config;
mod headless;
mod install;
mod keys;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-forge-bitbucket",
    version,
    about = "Bitbucket Cloud PR viewer for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit. Hits the API
    /// to verify the app password works (`/2.0/user`).
    #[arg(long)]
    check: bool,
    /// #1103 f/u7 (2026-08-20) — dump a human-readable diagnostic
    /// report to stdout, then exit. Covers auth resolution, whoami
    /// live-test, config summary, and version info. Powers mnml's
    /// `integrations.diag` palette command + the "Run diagnostics"
    /// chip context menu. Distinct from `--check` (which is a
    /// simple pass/fail probe): `--diag` is a full tree of state
    /// intended for support conversations.
    #[arg(long)]
    diag: bool,
    /// Headless: print every open PR the configured PR tabs would
    /// surface, as JSON on stdout, then exit. Used by mnml's
    /// `pr.picker` cross-host palette command and by the rail's
    /// "Open PRs" subsection refresh. Requires `--json` (only
    /// shape supported v1).
    #[arg(long)]
    list_prs: bool,
    /// Headless: print the URL of the most recent pipeline run on
    /// `--branch` in `--owner/--repo`, as `{"url": "..."}` JSON on
    /// stdout. Used by mnml's pr.picker Tab → cross-nav. Returns
    /// `{"url": null}` when no matching pipeline is found.
    #[arg(long)]
    find_pipeline_for_pr: bool,
    /// Owner (workspace) for `--find-pipeline-for-pr`.
    #[arg(long)]
    owner: Option<String>,
    /// Repo for `--find-pipeline-for-pr`.
    #[arg(long)]
    repo: Option<String>,
    /// Source branch name for `--find-pipeline-for-pr`.
    #[arg(long)]
    branch: Option<String>,
    /// Required for `--list-prs` / `--find-pipeline-for-pr`. Reserves
    /// the headless surface for future shapes.
    #[arg(long)]
    json: bool,
    /// Headless: emit statusline-segment values as JSON on stdout,
    /// then exit. Powers mnml 0.2.11+'s generic
    /// `[[values_sources]]` polling — mnml runs this on a cadence
    /// and paints the result through a matching `[[statusline_segments]]`
    /// chip. Emits `{"open_mine": N, "approved_mine": K}` where
    /// N = count of OPEN PRs authored by the auth user across the
    /// configured workspace, K = subset that have at least one
    /// approval. Exits non-zero with a short stderr message on
    /// auth / network / timeout failure — mnml then renders `!`
    /// on the chip.
    #[arg(long)]
    values: bool,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/bitbucket.toml so the
    /// rail chip + palette command + <leader>ib chord appear on the
    /// next mnml startup (or after `integrations.refresh`).
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest for this sibling. Rail
    /// chip / commands / chord disappear on the next mnml restart.
    #[arg(long)]
    uninstall: bool,
    /// Filter the interactive TUI to a single family of tabs and hide
    /// the tab strip. Powers mnml's split "Bitbucket Pull Requests"
    /// and "Bitbucket Pipelines" chips so each drops the user
    /// straight into a single-purpose view.
    ///
    /// Values (post tree-redesign schema):
    ///   `prs`       → workspace_open_prs + workspace_merged_prs +
    ///                 legacy pull_requests kinds
    ///   `pipelines` → workspace_pipelines + legacy pipelines kinds
    ///   `branches`  → legacy branches kind
    #[arg(long)]
    only: Option<String>,
    /// #1117 (2026-08-21) — mnml core's prefetch worker invokes
    /// `mnml-forge-bitbucket --prefetch --only <kind>` on a background
    /// cadence and stashes stdout under
    /// `~/.cache/mnml/prefetch/bitbucket_prs-<id>.json`. When the user
    /// then opens the corresponding pane, mnml passes the cache path
    /// via `MNML_PREFETCH_CACHE_FILE` and the interactive launch
    /// hydrates from it instead of doing a cold Bitbucket fetch — the
    /// pane paints populated on frame one.
    ///
    /// Emits a JSON object of shape:
    ///   { "generated_at": <unix_secs>,
    ///     "tabs": [
    ///       { "name": "Open PRs", "kind": "RepoPrTree",
    ///         "rows": [ /* RepoPrs */ ] },
    ///       { "name": "Pipelines", "kind": "RepoTree",
    ///         "rows": [ /* RepoPipelines */ ] },
    ///       ...
    ///     ] }
    ///
    /// Runs headless — no TUI. Same 10s timeout as `--values` so a
    /// slow Bitbucket doesn't wedge the worker.
    #[arg(long)]
    prefetch: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // --install / --uninstall run before auth so the sibling
    // works before the user has set up the app password.
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }

    // #1099 f/u loader (2026-08-20) — Pty spawn + config load +
    // whoami + repo enumeration + first PR fetch takes 1-3s on
    // large workspaces. Emit a status line to stdout NOW so the
    // Pty pane renders "loading…" instantly instead of appearing
    // frozen. Guarded on interactive shape (skip in headless
    // paths — --values / --list-prs / --find-pipeline-for-pr /
    // --install / --uninstall — where the JSON contract matters).
    if !cli.values
        && !cli.list_prs
        && !cli.find_pipeline_for_pr
        && !cli.install
        && !cli.uninstall
        && !cli.check
        && !cli.diag
        && !cli.prefetch
    {
        eprintln!("Bitbucket · loading…");
    }

    // Config first so first-run users get the scaffold-template
    // path before being asked for an app password.
    let mut cfg = config::load()?;

    // `--only <family>` — filter cfg.tabs down to a single family and
    // signal the TUI to skip its tab strip.
    //
    // #1099 (2026-08-20) — added `prs-mine` as a further-narrowed
    // variant. `--only prs` matches any PR-family tab; `--only
    // prs-mine` additionally restricts to `mode = "mine"` tabs so
    // the statusline chip's click semantic ("open PRs I authored")
    // lands directly on the matching tab instead of the whole PR
    // family (which shows every PR in the workspace).
    let force_hide_strip = if let Some(kind_str) = cli.only.as_deref() {
        let allowed: &[&str] = match kind_str {
            "prs" | "pull_requests" | "prs-mine" => &[
                "workspace_open_prs",
                "workspace_merged_prs",
                "pull_requests",
            ],
            "pipelines" => &["workspace_pipelines", "pipelines"],
            "branches" => &["branches"],
            other => anyhow::bail!(
                "--only {other:?} unrecognized (want `prs` | `prs-mine` | `pipelines` | `branches`)"
            ),
        };
        let before = cfg.tabs.len();
        cfg.tabs.retain(|t| allowed.contains(&t.kind.as_str()));
        if kind_str == "prs-mine" {
            // #1099 addendum 2 (2026-08-20) — if the user's config
            // has PR tabs but none with `mode = "mine"`, don't
            // hard-fail AND don't silently expand to the full PR
            // family (prior fallback broke the chip's semantic
            // promise — "click me to see MY PRs" landed on 56 PRs
            // across every author). Instead, synthesize a mine tab
            // in-memory so the chip click always lands on mine-only.
            let mine_tabs: Vec<_> = cfg
                .tabs
                .iter()
                .filter(|t| t.mode.as_deref() == Some("mine"))
                .cloned()
                .collect();
            if mine_tabs.is_empty() {
                // Use workspace_open_prs with mine_only=true so the
                // pane renders as the tree-grouped RepoPrTree view
                // (matching the user's Bitbucket PRs pane muscle
                // memory) filtered to my authored PRs. Sorted by
                // updated_on desc + state=OPEN are intrinsic to that
                // kind — no extra config needed.
                cfg.tabs = vec![config::Tab {
                    name: "Mine".into(),
                    kind: "workspace_open_prs".into(),
                    workspace: None, // inherits cfg.workspace
                    repo: None,
                    state: "OPEN".into(),
                    mode: None,
                    q: None,
                    mine_only: true,
                }];
            } else {
                cfg.tabs = mine_tabs;
            }
        }
        if cfg.tabs.is_empty() {
            anyhow::bail!(
                "--only {kind_str}: no tabs of that family in {} (had {before} tabs total; check the [[tabs]] entries and their `kind =` field)",
                config::config_path().display()
            );
        }
        true
    } else {
        false
    };
    let (token, token_source) = auth::load_token().with_context(|| {
        format!(
            "couldn't load Bitbucket token (tried env BITBUCKET_API_TOKEN, BITBUCKET_APP_PASSWORD, BITBUCKET_PERSONAL_TOKEN, then {})",
            auth::token_path().display()
        )
    })?;
    let client = bitbucket::Client::new(&cfg.email, &token)?;

    if cli.list_prs {
        if !cli.json {
            anyhow::bail!("--list-prs requires --json (only shape supported v1)");
        }
        return headless::list_prs(&cfg, &client).await;
    }

    if cli.values {
        // No `--json` requirement — the flag's whole purpose is
        // emitting a JSON blob for mnml's statusline-segment poller
        // (see mnml/src/app/statusline_segments.rs). Wrap in a 10s
        // timeout so a hung network never freezes the polling
        // worker on the mnml side (worker interval is >=30s; a poll
        // that takes longer than the interval starves the next
        // one). Any failure — timeout / auth / network / parse —
        // exits non-zero with a short stderr message; mnml
        // renders `!` on the chip and surfaces the message via
        // hover.
        let fut = headless::list_values(&cfg, &client);
        return match tokio::time::timeout(std::time::Duration::from_secs(10), fut).await {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!("--values timed out after 10s")),
        };
    }

    if cli.prefetch {
        // #1117 (2026-08-21) — background prefetch producer. Build
        // the App the same way an interactive launch would (so all
        // tabs are resolved + startup-prefetch fetches the same
        // rows the pane will show), then emit each tab's data as
        // JSON for mnml's prefetch worker to cache. 10s timeout
        // matches --values so a slow Bitbucket doesn't wedge the
        // worker.
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
            "token source: {token_source} (loaded, {} chars)",
            token.len()
        );
        println!("workspace: {}", cfg.workspace);
        println!("email: {}", cfg.email);
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        println!("scope: {}", cfg.scope);
        println!("recent_window_days: {}", cfg.recent_window_days);
        match client.whoami().await {
            Ok(u) => println!(
                "whoami: ok — {} (account_id: {})",
                u.display_name,
                u.account_id.as_deref().unwrap_or("<none>")
            ),
            Err(e) => println!("whoami: FAIL — {e}"),
        }
        for (i, t) in cfg.tabs.iter().enumerate() {
            let shape = match (&t.mode, &t.repo, &t.q) {
                (Some(m), _, _) => format!("mode={m}"),
                (None, Some(r), _) => format!("repo={r}"),
                (None, None, Some(_)) => "q=<custom>".to_string(),
                _ => format!("kind={}", t.kind),
            };
            println!("  tab {} ({}): {shape}, state={}", i + 1, t.name, t.state);
        }

        // tree-redesign 2026-07-15 diagnostic — actually exercise
        // the workspace-wide fetches so we can tell whether "0 PRs"
        // is because there truly are 0, because the fetch errored,
        // or because the scope filter dropped everything.
        println!("\n── live workspace fetch ──");
        match client
            .list_workspace_repos_with_activity(&cfg.workspace)
            .await
        {
            Ok(rs) => {
                println!("repos_with_activity: {} total", rs.len());
                for r in rs.iter().take(3) {
                    println!(
                        "  {} — updated_on={:?}",
                        r.slug,
                        r.updated_on.as_deref().unwrap_or("<none>")
                    );
                }
                // Use the first 3 repos for the workspace fan-outs
                // — small enough that failures surface fast and 3
                // repos is plenty to prove the code path works.
                let slugs: Vec<String> = rs.iter().take(3).map(|r| r.slug.clone()).collect();

                println!("\nOPEN PRs (first 3 repos = {:?}):", slugs);
                match client
                    .list_workspace_open_and_draft_prs(&cfg.workspace, &slugs, 25)
                    .await
                {
                    Ok(prs) => {
                        println!("  ok, {} prs", prs.len());
                        for pr in prs.iter().take(5) {
                            println!("    #{} {}", pr.id, pr.title);
                        }
                    }
                    Err(e) => println!("  ERR: {e}"),
                }

                println!("\nMERGED PRs (same 3 repos):");
                match client
                    .list_workspace_merged_prs(&cfg.workspace, &slugs, 3)
                    .await
                {
                    Ok(prs) => println!("  ok, {} prs", prs.len()),
                    Err(e) => println!("  ERR: {e}"),
                }

                println!("\nPIPELINES tree (same 3 repos):");
                match client
                    .list_workspace_pipelines_tree(&cfg.workspace, &slugs, 5, 25)
                    .await
                {
                    Ok(t) => {
                        for r in t {
                            println!(
                                "  {} — {} branches, first branch has pipeline: {}",
                                r.slug,
                                r.branches.len(),
                                r.branches
                                    .first()
                                    .map(|b| b.latest_pipeline.is_some())
                                    .unwrap_or(false)
                            );
                        }
                    }
                    Err(e) => println!("  ERR: {e}"),
                }
            }
            Err(e) => println!("repos_with_activity: ERR: {e}"),
        }

        return Ok(());
    }

    if cli.diag {
        return run_diag(&cfg, &client, &token_source.to_string(), token.len()).await;
    }

    let mut app = app::App::new(cfg, client).await?;
    app.hide_tab_strip = force_hide_strip;

    ui::run(&mut app).await
}

/// #1103 f/u7 (2026-08-20) — human-readable diagnostic dump.
/// Structured tree format so `mnml-forge-bitbucket --diag` output
/// is scannable in a Pty pane or a shell. Sections cover auth
/// (source + live whoami test), config summary, and runtime info.
/// Every section runs independently so a failure in one doesn't
/// suppress the rest — the user gets to see WHY something broke
/// alongside what's working.
async fn run_diag(
    cfg: &config::Config,
    client: &bitbucket::Client,
    token_source: &str,
    token_len: usize,
) -> Result<()> {
    println!("mnml-forge-bitbucket · diagnostics");
    println!();
    println!("Auth");
    println!("  ├─ source: {token_source}");
    println!("  ├─ token length: {token_len} chars");
    println!("  ├─ email: {}", cfg.email);
    match client.whoami().await {
        Ok(u) => {
            println!("  └─ whoami: ✓ {}", u.display_name);
            println!(
                "     account_id: {}",
                u.account_id.as_deref().unwrap_or("<none returned>")
            );
        }
        Err(e) => {
            println!("  └─ whoami: ✗ {e}");
            println!("     mine-only filters, --values, and workspace repo enumeration all depend on this succeeding.");
        }
    }
    println!();
    println!("Config");
    println!("  ├─ path: {}", config::config_path().display());
    println!("  ├─ workspace: {}", cfg.workspace);
    println!("  ├─ scope: {}", cfg.scope);
    println!("  ├─ recent_window_days: {}", cfg.recent_window_days);
    println!("  ├─ refresh_interval_secs: {}", cfg.refresh_interval_secs);
    if cfg.repos.is_empty() {
        println!("  ├─ repos allowlist: (none — enumerating all)");
    } else {
        println!("  ├─ repos allowlist: {} entries", cfg.repos.len());
        for r in cfg.repos.iter().take(5) {
            println!("  │   {r}");
        }
        if cfg.repos.len() > 5 {
            println!("  │   … and {} more", cfg.repos.len() - 5);
        }
    }
    println!("  └─ tabs: {}", cfg.tabs.len());
    for (i, t) in cfg.tabs.iter().enumerate() {
        let shape = match (&t.mode, &t.repo, &t.q) {
            (Some(m), _, _) => format!("mode={m}"),
            (None, Some(r), _) => format!("repo={r}"),
            (None, None, Some(_)) => "q=<custom>".into(),
            _ => format!("kind={}", t.kind),
        };
        println!("      {}. {} ({shape}, state={})", i + 1, t.name, t.state);
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

/// #1117 (2026-08-21) — background prefetch producer. Constructs
/// the same App the interactive launch would (so tab resolution +
/// startup-prefetch match exactly), then serializes each tab's row
/// list as JSON. mnml core caches stdout to
/// `~/.cache/mnml/prefetch/<int>-<id>.json` and stamps the path on
/// the child env via `MNML_PREFETCH_CACHE_FILE` when the pane
/// opens. The interactive launch checks that env in `App::new` +
/// hydrates from JSON instead of doing a cold fetch — the pane
/// paints populated on frame one.
async fn run_prefetch(cfg: config::Config, client: bitbucket::Client) -> Result<()> {
    use app::TabData;
    #[derive(serde::Serialize)]
    struct PrefetchCache {
        generated_at: u64,
        tabs: Vec<PrefetchTab>,
    }
    #[derive(serde::Serialize)]
    struct PrefetchTab {
        name: String,
        /// TabData variant discriminant so the hydrator on the other
        /// side can pick the right shape. One of "PullRequests" /
        /// "Pipelines" / "Branches" / "RepoTree" / "RepoPrTree".
        kind: &'static str,
        /// Homogeneous JSON payload per variant — see `kind` for the
        /// concrete element type. Empty vec on empty / failed tabs so
        /// the hydrator populates the pane with a real (empty)
        /// state, not a stale one.
        rows: serde_json::Value,
    }
    // App::new already walks every configured tab and refreshes it
    // in the startup-prefetch loop (see app.rs). That fetches all
    // tabs the pane will show — no extra walk needed here.
    let app = app::App::new(cfg, client).await?;
    let tabs: Vec<PrefetchTab> = app
        .tabs
        .iter()
        .map(|t| {
            let (kind, rows) = match &t.data {
                TabData::PullRequests(v) => (
                    "PullRequests",
                    serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                ),
                TabData::Pipelines(v) => (
                    "Pipelines",
                    serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                ),
                TabData::Branches(v) => (
                    "Branches",
                    serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                ),
                TabData::RepoTree { rows, .. } => (
                    "RepoTree",
                    serde_json::to_value(rows).unwrap_or(serde_json::Value::Null),
                ),
                TabData::RepoPrTree { rows, .. } => (
                    "RepoPrTree",
                    serde_json::to_value(rows).unwrap_or(serde_json::Value::Null),
                ),
            };
            PrefetchTab {
                name: t.name.clone(),
                kind,
                rows,
            }
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
