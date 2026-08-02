//! Headless JSON modes for cross-host integration with mnml.
//!
//! Two CLI flags trigger these:
//!
//! - `--list-prs --json` — print every open PR the configured Issues
//!   tabs would surface, deduped by html_url, in the cross-host JSON
//!   schema documented in mnml's pr-picker design. `source_branch`
//!   is left null — the Issues API doesn't return head.ref; mnml's
//!   cross-nav falls back to the most-recent workflow run for the
//!   repo when source_branch is null.
//! - `--find-pipeline-for-pr --owner <o> --repo <r> --branch <b>` —
//!   return the URL of the most recent Actions workflow run on
//!   `<branch>` in `<owner>/<repo>`, or `null`.

use crate::config::Config;
use crate::github::{Client, Issue};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashSet;

#[derive(Serialize)]
struct PrJson {
    id: String,
    url: String,
    owner: String,
    repo: String,
    title: String,
    author: String,
    /// Issues API doesn't include head.ref. mnml's cross-nav treats
    /// null as "open Actions for the repo, most-recent run".
    source_branch: Option<String>,
    /// Same caveat as source_branch.
    dest_branch: Option<String>,
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

pub async fn list_prs(cfg: &Config, client: &Client) -> Result<()> {
    let mut all: Vec<Issue> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for t in cfg.tabs.iter().filter(|t| t.kind == "issues") {
        let Some(query) = t.query.as_deref() else {
            continue;
        };
        match client.search(query, 50).await {
            Ok(items) => {
                for item in items.into_iter().filter(Issue::is_pr) {
                    if seen.insert(item.html_url.clone()) {
                        all.push(item);
                    }
                }
            }
            Err(e) => {
                eprintln!("tab '{}' skipped: {e:#}", t.name);
            }
        }
    }

    let result = ListPrsResult {
        host: "github",
        prs: all.iter().map(pr_to_json).collect(),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn pr_to_json(item: &Issue) -> PrJson {
    let full = item.repo_short();
    let (owner, repo) = full
        .split_once('/')
        .map(|(o, r)| (o.to_string(), r.to_string()))
        .unwrap_or_else(|| (String::new(), full.clone()));
    PrJson {
        id: item.number.to_string(),
        url: item.html_url.clone(),
        owner: owner.clone(),
        repo: repo.clone(),
        title: item.title.clone(),
        author: item
            .user
            .as_ref()
            .map(|u| u.login.clone())
            .unwrap_or_default(),
        source_branch: None,
        dest_branch: None,
        state: item.state.to_lowercase(),
        updated_at: item.updated_at.clone().unwrap_or_default(),
        remote_url_https: format!("https://github.com/{owner}/{repo}.git"),
        remote_url_ssh: format!("git@github.com:{owner}/{repo}.git"),
    }
}

#[derive(Serialize)]
struct PipelineResult {
    url: Option<String>,
}

/// Look up the most-recent Actions workflow run on `branch` in
/// `owner/repo`. Returns the GitHub html_url or null.
pub async fn find_pipeline_for_pr(
    client: &Client,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<()> {
    let runs = client
        .actions_runs(owner, repo, Some(branch), 20)
        .await
        .with_context(|| format!("listing actions for {owner}/{repo}"))?;
    let url = runs.first().map(|r| r.html_url.clone());
    let result = PipelineResult { url };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
