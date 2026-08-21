//! Minimal GitHub REST API client.
//!
//! Two endpoints are wired:
//!   - Issues search (`GET /search/issues`) — covers issues AND PRs.
//!     Docs: <https://docs.github.com/en/rest/search/search>
//!   - Actions runs (`GET /repos/{owner}/{repo}/actions/runs`).
//!     Docs: <https://docs.github.com/en/rest/actions/workflow-runs>
//!
//! Auth is the same `Authorization: Bearer <token>` for both.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

const SEARCH_ENDPOINT: &str = "https://api.github.com/search/issues";

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    token: String,
}

impl Client {
    pub fn new(token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("mnml-forge-github/0.2.0")
            .build()?;
        Ok(Self {
            http,
            token: token.to_string(),
        })
    }

    /// #1103 f/u7 (2026-08-20) — verify the current auth token by
    /// calling `GET /user`. Returns the authenticated user's login
    /// on success; errors surface HTTP status + body for `--diag`.
    pub async fn whoami(&self) -> Result<String> {
        let resp = self
            .http
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("GitHub /user request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitHub /user failed: {status}: {text}"));
        }
        #[derive(Deserialize)]
        struct WhoamiResp {
            login: String,
        }
        let u: WhoamiResp = resp.json().await.context("parsing /user response")?;
        Ok(u.login)
    }

    /// Run a GitHub issue-search query. Returns up to `per_page`
    /// results (the API caps this at 100). Pagination isn't wired
    /// in v0.2 — first page only.
    pub async fn search(&self, query: &str, per_page: u32) -> Result<Vec<Issue>> {
        let resp = self
            .http
            .get(SEARCH_ENDPOINT)
            .query(&[
                ("q", query),
                ("per_page", &per_page.to_string()),
                ("sort", "updated"),
                ("order", "desc"),
            ])
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("GitHub search request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitHub search failed: {status}: {text}"));
        }
        let sr: SearchResult = resp
            .json()
            .await
            .context("parsing GitHub search response")?;
        Ok(sr.items)
    }

    /// Recent workflow runs for `owner/repo`. Optionally filter by
    /// branch (None ⇒ all branches). Returns up to `per_page`
    /// runs, ordered server-side by created_at descending.
    pub async fn actions_runs(
        &self,
        owner: &str,
        repo: &str,
        branch: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<WorkflowRun>> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/actions/runs");
        let per_page_s = per_page.to_string();
        let mut query: Vec<(&str, &str)> = vec![("per_page", per_page_s.as_str())];
        if let Some(b) = branch {
            query.push(("branch", b));
        }
        let resp = self
            .http
            .get(&url)
            .query(&query)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("GitHub actions request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitHub actions failed: {status}: {text}"));
        }
        let rr: ActionsResponse = resp
            .json()
            .await
            .context("parsing GitHub actions response")?;
        Ok(rr.workflow_runs)
    }
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    items: Vec<Issue>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    pub html_url: String,
    pub state: String,
    /// `pull_request` is present (even if just `{}`) when this is a
    /// PR rather than an issue. Used to badge rows.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub assignee: Option<User>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// `repository_url` is `https://api.github.com/repos/{owner}/{name}`.
    /// We strip the prefix to render the `owner/name` chip in the table.
    #[serde(default)]
    pub repository_url: Option<String>,
}

impl Issue {
    pub fn is_pr(&self) -> bool {
        self.pull_request.is_some()
    }

    /// `owner/name` derived from `repository_url`. Falls back to the
    /// trailing two URL segments of `html_url` (which always has
    /// `https://github.com/<owner>/<name>/{issues,pull}/<n>`).
    pub fn repo_short(&self) -> String {
        if let Some(url) = &self.repository_url
            && let Some(idx) = url.find("/repos/")
        {
            return url[idx + 7..].to_string();
        }
        // Fallback path.
        let parts: Vec<&str> = self.html_url.split('/').collect();
        if parts.len() >= 5 {
            format!("{}/{}", parts[3], parts[4])
        } else {
            String::new()
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Label {
    pub name: String,
    /// 6-hex color code without the leading `#`.
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Deserialize)]
struct ActionsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

/// One Actions workflow run row. Mirrors the
/// `/repos/{owner}/{repo}/actions/runs` response shape; we only
/// pull the fields we render.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct WorkflowRun {
    pub id: i64,
    pub name: Option<String>,
    pub display_title: Option<String>,
    pub html_url: String,
    /// `queued`, `in_progress`, `completed`.
    pub status: String,
    /// `success`, `failure`, `cancelled`, `skipped`, `neutral`, …
    /// `None` while the run is still queued/in_progress.
    #[serde(default)]
    pub conclusion: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub event: String,
    pub run_number: i64,
    pub run_attempt: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub actor: Option<User>,
}

impl WorkflowRun {
    /// Short status chip — what to render in the conclusion column.
    /// Returns `"running"` / `"queued"` while still in flight.
    pub fn status_chip(&self) -> &str {
        match self.status.as_str() {
            "completed" => self.conclusion.as_deref().unwrap_or("done"),
            "in_progress" => "running",
            "queued" => "queued",
            s => s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_short_extracts_owner_name_from_repository_url() {
        let i = Issue {
            number: 1,
            title: "t".into(),
            html_url: "https://github.com/owner/name/issues/1".into(),
            state: "open".into(),
            pull_request: None,
            user: None,
            assignee: None,
            labels: vec![],
            updated_at: None,
            repository_url: Some("https://api.github.com/repos/owner/name".into()),
        };
        assert_eq!(i.repo_short(), "owner/name");
    }

    #[test]
    fn repo_short_falls_back_to_html_url() {
        let i = Issue {
            number: 42,
            title: "t".into(),
            html_url: "https://github.com/owner/name/issues/42".into(),
            state: "open".into(),
            pull_request: None,
            user: None,
            assignee: None,
            labels: vec![],
            updated_at: None,
            repository_url: None,
        };
        assert_eq!(i.repo_short(), "owner/name");
    }

    #[test]
    fn is_pr_true_when_pull_request_field_present() {
        let i = Issue {
            number: 1,
            title: "t".into(),
            html_url: "https://github.com/owner/name/pull/1".into(),
            state: "open".into(),
            pull_request: Some(serde_json::json!({})),
            user: None,
            assignee: None,
            labels: vec![],
            updated_at: None,
            repository_url: None,
        };
        assert!(i.is_pr());
    }

    #[test]
    fn status_chip_uses_conclusion_when_completed() {
        let r = WorkflowRun {
            id: 1,
            name: None,
            display_title: None,
            html_url: "".into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
            head_branch: None,
            head_sha: "abc".into(),
            event: "push".into(),
            run_number: 1,
            run_attempt: None,
            created_at: None,
            updated_at: None,
            actor: None,
        };
        assert_eq!(r.status_chip(), "success");
    }

    #[test]
    fn status_chip_running_when_in_progress() {
        let r = WorkflowRun {
            id: 1,
            name: None,
            display_title: None,
            html_url: "".into(),
            status: "in_progress".into(),
            conclusion: None,
            head_branch: None,
            head_sha: "abc".into(),
            event: "push".into(),
            run_number: 1,
            run_attempt: None,
            created_at: None,
            updated_at: None,
            actor: None,
        };
        assert_eq!(r.status_chip(), "running");
    }
}
