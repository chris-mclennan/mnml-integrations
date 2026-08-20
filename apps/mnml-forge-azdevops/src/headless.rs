//! Headless JSON modes for cross-host integration with mnml.

use crate::azdevops::{Build, Client, PullRequest};
use crate::config::Config;
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
    source_branch: Option<String>,
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
    let mut all: Vec<(String, String, PullRequest)> = Vec::new();
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    let mut user_guid: Option<(String, String)> = None; // (org, guid)

    for t in cfg.tabs.iter().filter(|t| t.kind == "pull_requests") {
        let org = t.org.as_deref().unwrap_or(&cfg.org);
        let Some(project) = t.project.as_deref().or(cfg.project.as_deref()) else {
            eprintln!("tab '{}' skipped: no project", t.name);
            continue;
        };

        let res: Result<Vec<PullRequest>> = match (t.mode.as_deref(), t.repo.as_deref()) {
            (Some(mode), _) if mode == "mine" || mode == "reviewing" => {
                let guid = match &user_guid {
                    Some((cached_org, g)) if cached_org == org => g.clone(),
                    _ => match client.connection_data(org).await {
                        Ok(cd) => {
                            let g = cd.authenticated_user.id;
                            user_guid = Some((org.to_string(), g.clone()));
                            g
                        }
                        Err(e) => {
                            eprintln!("tab '{}' skipped: connection_data: {e:#}", t.name);
                            continue;
                        }
                    },
                };
                let param = if mode == "mine" {
                    "searchCriteria.creatorId"
                } else {
                    "searchCriteria.reviewerId"
                };
                client
                    .pull_requests_by_person(org, project, param, &guid, &t.state, 50)
                    .await
            }
            (None, Some(repo)) => {
                client
                    .pull_requests_repo(org, project, repo, &t.state, 50)
                    .await
            }
            _ => {
                eprintln!("tab '{}' skipped: needs mode or repo", t.name);
                continue;
            }
        };
        match res {
            Ok(prs) => {
                for pr in prs {
                    if seen.insert((org.to_string(), pr.id)) {
                        all.push((org.to_string(), project.to_string(), pr));
                    }
                }
            }
            Err(e) => eprintln!("tab '{}' skipped: {e:#}", t.name),
        }
    }

    let result = ListPrsResult {
        host: "azdevops",
        prs: all
            .iter()
            .map(|(org, project, pr)| pr_to_json(org, project, pr))
            .collect(),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn pr_to_json(org: &str, project: &str, pr: &PullRequest) -> PrJson {
    let repo_name = pr
        .repository
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    let owner = format!("{org}/{project}");
    let url = pr.web_url(org, project);
    let source = pr.source_branch_short();
    let dest = pr.target_branch_short();
    PrJson {
        id: pr.id.to_string(),
        url,
        owner: owner.clone(),
        repo: repo_name.clone(),
        title: pr.title.clone(),
        author: pr
            .created_by
            .as_ref()
            .map(|i| i.display_name.clone())
            .unwrap_or_default(),
        source_branch: if source.is_empty() {
            None
        } else {
            Some(source)
        },
        dest_branch: if dest.is_empty() { None } else { Some(dest) },
        state: pr.status.clone(),
        updated_at: pr.creation_date.clone().unwrap_or_default(),
        remote_url_https: format!("https://dev.azure.com/{org}/{project}/_git/{repo_name}"),
        // Azure DevOps SSH form for the visualstudio.com SSH hostname.
        remote_url_ssh: format!("git@ssh.dev.azure.com:v3/{org}/{project}/{repo_name}"),
    }
}

#[derive(Serialize)]
struct PipelineResult {
    url: Option<String>,
}

pub async fn find_pipeline_for_pr(
    client: &Client,
    org: &str,
    project: &str,
    repo: &str,
    branch: &str,
) -> Result<()> {
    let builds = client
        .builds(org, project, Some(repo), Some(branch), None, 20)
        .await
        .with_context(|| format!("listing builds for {org}/{project}/{repo}"))?;
    let url = builds.first().and_then(|b| build_url(b, org, project));
    let result = PipelineResult { url };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn build_url(b: &Build, org: &str, project: &str) -> Option<String> {
    Some(format!(
        "https://dev.azure.com/{org}/{project}/_build/results?buildId={}",
        b.id
    ))
}
