//! Headless JSON modes for cross-host integration with mnml.
//!
//! Two CLI flags trigger these, both invoked by mnml's `pr.picker`
//! palette command and the rail "Open PRs" refresh:
//!
//! - `--list-prs --json` — print every open PR the configured PR tabs
//!   would surface, deduped by `(workspace, repo, id)`, in the
//!   cross-host JSON schema documented in mnml's pr-picker design.
//! - `--find-pipeline-for-pr --owner <o> --repo <r> --branch <b>` —
//!   return the URL of the most recent pipeline run on `<branch>` in
//!   `<owner>/<repo>`, or `null`.
//!
//! Both write one JSON object to stdout, log to stderr, and exit 0 on
//! success (even when the result list is empty). Auth / network
//! failures exit non-zero.

use crate::bitbucket::{Client, Pipeline, PullRequest};
use crate::config::{Config, Tab};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashSet;

/// One PR in the cross-host JSON schema (matches mnml's `SiblingPr`).
#[derive(Serialize)]
struct PrJson {
    id: String,
    url: String,
    owner: String,
    repo: String,
    title: String,
    author: String,
    source_branch: String,
    dest_branch: String,
    state: String,
    updated_at: String,
    remote_url_https: String,
    remote_url_ssh: String,
}

#[derive(Serialize)]
struct ListPrsResult {
    host: &'static str,
    prs: Vec<PrJson>,
}

/// Walk each configured PR tab, fetch open PRs, dedupe, emit JSON.
/// Skips Pipelines / Branches tabs. Per-tab errors land on stderr; one
/// broken tab doesn't tank the whole list.
pub async fn list_prs(cfg: &Config, client: &Client) -> Result<()> {
    let mut all: Vec<PullRequest> = Vec::new();
    let mut seen: HashSet<(String, String, i64)> = HashSet::new();

    for t in cfg.tabs.iter().filter(|t| t.kind == "pull_requests") {
        let workspace = t.workspace.as_deref().unwrap_or(&cfg.workspace);
        match fetch_tab(client, workspace, t).await {
            Ok(prs) => {
                for pr in prs {
                    let (ws, rp) = split_workspace_repo(&pr);
                    if seen.insert((ws.clone(), rp.clone(), pr.id)) {
                        all.push(pr);
                    }
                }
            }
            Err(e) => {
                eprintln!("tab '{}' skipped: {e:#}", t.name);
            }
        }
    }

    let result = ListPrsResult {
        host: "bitbucket",
        prs: all.iter().map(pr_to_json).collect(),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

async fn fetch_tab(client: &Client, workspace: &str, t: &Tab) -> Result<Vec<PullRequest>> {
    // Headless v1 covers per-repo tabs only. `mode = mine`/`reviewing`
    // without an explicit `repo` would need a workspace-wide PR query
    // that BB Cloud's API doesn't directly expose for the auth user —
    // those tabs are skipped with a stderr note; users who want them
    // can configure per-repo tabs alongside.
    let Some(repo) = t.repo.as_deref() else {
        anyhow::bail!("no `repo` field — headless lists are per-repo only");
    };
    client
        .list_repo_prs(workspace, repo, Some(&t.state), t.q.as_deref(), 50)
        .await
}

fn split_workspace_repo(pr: &PullRequest) -> (String, String) {
    let full = pr.repo_short();
    full.split_once('/')
        .map(|(w, r)| (w.to_string(), r.to_string()))
        .unwrap_or_else(|| (String::new(), full.clone()))
}

fn pr_to_json(pr: &PullRequest) -> PrJson {
    let (owner, repo) = split_workspace_repo(pr);
    let source_branch = pr
        .source
        .as_ref()
        .and_then(|b| b.branch.as_ref())
        .map(|b| b.name.clone())
        .unwrap_or_default();
    let dest_branch = pr
        .destination
        .as_ref()
        .and_then(|b| b.branch.as_ref())
        .map(|b| b.name.clone())
        .unwrap_or_default();
    let url = pr
        .html_url()
        .unwrap_or_else(|| format!("https://bitbucket.org/{owner}/{repo}/pull-requests/{}", pr.id));
    PrJson {
        id: pr.id.to_string(),
        url,
        owner: owner.clone(),
        repo: repo.clone(),
        title: pr.title.clone(),
        author: pr
            .author
            .as_ref()
            .map(|u| u.display_name.clone())
            .unwrap_or_default(),
        source_branch,
        dest_branch,
        state: pr.state.to_lowercase(),
        updated_at: pr.updated_on.clone().unwrap_or_default(),
        remote_url_https: format!("https://bitbucket.org/{owner}/{repo}.git"),
        remote_url_ssh: format!("git@bitbucket.org:{owner}/{repo}.git"),
    }
}

#[derive(Serialize)]
struct PipelineResult {
    url: Option<String>,
}

/// Look up the most-recent pipeline run on `branch` in `owner/repo`.
/// Returns the bitbucket.org URL, or `null` when no pipeline matched.
pub async fn find_pipeline_for_pr(
    client: &Client,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<()> {
    let pipelines = client
        .list_pipelines(owner, repo, 50)
        .await
        .with_context(|| format!("listing pipelines for {owner}/{repo}"))?;
    let url = pipelines
        .iter()
        .find(|p| matches_branch(p, branch))
        .map(|p| pipeline_url(owner, repo, p));
    let result = PipelineResult { url };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn matches_branch(p: &Pipeline, branch: &str) -> bool {
    p.target
        .as_ref()
        .and_then(|t| t.ref_name.as_deref())
        .map(|r| r == branch)
        .unwrap_or(false)
}

fn pipeline_url(owner: &str, repo: &str, p: &Pipeline) -> String {
    format!(
        "https://bitbucket.org/{owner}/{repo}/pipelines/results/{}",
        p.build_number
    )
}
