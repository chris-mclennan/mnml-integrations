//! Headless JSON modes for cross-host integration with mnml.

use crate::config::Config;
use crate::gitlab::{Client, MergeRequest};
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
    let mut all: Vec<MergeRequest> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut user_id: Option<i64> = None;

    for t in cfg.tabs.iter().filter(|t| t.kind == "merge_requests") {
        let res: Result<Vec<MergeRequest>> = match (t.mode.as_deref(), t.project.as_deref()) {
            (Some(mode), _) if mode == "mine" || mode == "reviewing" => {
                if user_id.is_none() {
                    match client.whoami().await {
                        Ok(u) => user_id = Some(u.id),
                        Err(e) => {
                            eprintln!("tab '{}' skipped: whoami failed: {e:#}", t.name);
                            continue;
                        }
                    }
                }
                let param = if mode == "mine" {
                    "author_id"
                } else {
                    "reviewer_id"
                };
                client
                    .merge_requests_by_person(param, user_id.unwrap(), &t.state, 50)
                    .await
            }
            (None, Some(project)) => client.merge_requests_project(project, &t.state, 50).await,
            _ => {
                eprintln!("tab '{}' skipped: needs mode or project", t.name);
                continue;
            }
        };
        match res {
            Ok(mrs) => {
                for mr in mrs {
                    if seen.insert(mr.id) {
                        all.push(mr);
                    }
                }
            }
            Err(e) => eprintln!("tab '{}' skipped: {e:#}", t.name),
        }
    }

    let host = gitlab_host_from_base_url(&cfg.base_url);
    let result = ListPrsResult {
        host: "gitlab",
        prs: all.iter().map(|mr| mr_to_json(mr, &host)).collect(),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn mr_to_json(mr: &MergeRequest, host: &str) -> PrJson {
    let path = mr.project_path_from_url();
    let (owner, repo) = path
        .rsplit_once('/')
        .map(|(o, r)| (o.to_string(), r.to_string()))
        .unwrap_or_else(|| (String::new(), path.clone()));
    PrJson {
        id: mr.iid.to_string(),
        url: mr.web_url.clone(),
        owner,
        repo,
        title: mr.title.clone(),
        author: mr
            .author
            .as_ref()
            .map(|u| u.username.clone())
            .unwrap_or_default(),
        source_branch: mr.source_branch.clone(),
        dest_branch: mr.target_branch.clone(),
        state: mr.state.clone(),
        updated_at: mr.updated_at.clone().unwrap_or_default(),
        remote_url_https: format!("https://{host}/{path}.git"),
        remote_url_ssh: format!("git@{host}:{path}.git"),
    }
}

fn gitlab_host_from_base_url(base_url: &str) -> String {
    base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("gitlab.com")
        .to_string()
}

#[derive(Serialize)]
struct PipelineResult {
    url: Option<String>,
}

pub async fn find_pipeline_for_pr(
    client: &Client,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<()> {
    let project = if owner.is_empty() {
        repo.to_string()
    } else {
        format!("{owner}/{repo}")
    };
    let pipelines = client
        .pipelines(&project, Some(branch), 20)
        .await
        .with_context(|| format!("listing pipelines for {project}"))?;
    let url = pipelines.first().map(|p| p.web_url.clone());
    let result = PipelineResult { url };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
