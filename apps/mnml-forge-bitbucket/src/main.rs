mod app;
mod auth;
mod bitbucket;
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
    name = "mnml-forge-bitbucket",
    version,
    about = "Bitbucket Cloud PR viewer for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit. Hits the API
    /// to verify the app password works (`/2.0/user`).
    #[arg(long)]
    check: bool,
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

    let mut app = app::App::new(cfg, client).await?;
    app.hide_tab_strip = force_hide_strip;

    ui::run(&mut app).await
}
