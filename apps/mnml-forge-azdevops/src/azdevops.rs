//! Minimal Azure DevOps REST client. Three endpoints are wired:
//!
//!   - Pull requests (per-repo or workspace-spanning with
//!     `searchCriteria.creatorId` / `reviewerId`).
//!     <https://learn.microsoft.com/en-us/rest/api/azure/devops/git/pull-requests/get-pull-requests>
//!   - Builds (project-scoped, optional repo + branch).
//!     <https://learn.microsoft.com/en-us/rest/api/azure/devops/build/builds/list>
//!   - Connection data (resolves `authenticatedUser.id` for
//!     `mode = mine` / `reviewing` tabs).
//!     <https://learn.microsoft.com/en-us/rest/api/azure/devops/core/connection-data>
//!
//! Auth: HTTP Basic with an empty username and the PAT as the password
//! (Azure DevOps's standard PAT shape).

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde::Deserialize;

const API_VERSION: &str = "7.1";

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    basic_auth_header: String,
}

impl Client {
    pub fn new(token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("mnml-forge-azdevops/0.1.0")
            .build()?;
        let raw = format!(":{token}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let basic_auth_header = format!("Basic {encoded}");
        Ok(Self {
            http,
            basic_auth_header,
        })
    }

    /// Per-repo PR list — `GET /{org}/{project}/_apis/git/repositories/{repo}/pullrequests`.
    pub async fn pull_requests_repo(
        &self,
        org: &str,
        project: &str,
        repo: &str,
        status: &str,
        top: u32,
    ) -> Result<Vec<PullRequest>> {
        let url = format!(
            "https://dev.azure.com/{org}/{project}/_apis/git/repositories/{repo}/pullrequests"
        );
        let top_s = top.to_string();
        let mut q: Vec<(&str, &str)> = vec![
            ("api-version", API_VERSION),
            ("$top", top_s.as_str()),
        ];
        if status != "all" {
            q.push(("searchCriteria.status", status));
        }
        self.get_prs(&url, &q).await
    }

    /// Project-spanning PR list filtered by creator or reviewer.
    /// `who_param` is `"searchCriteria.creatorId"` or
    /// `"searchCriteria.reviewerId"`. `user_id` is the GUID
    /// returned by `connection_data`.
    pub async fn pull_requests_by_person(
        &self,
        org: &str,
        project: &str,
        who_param: &str,
        user_id: &str,
        status: &str,
        top: u32,
    ) -> Result<Vec<PullRequest>> {
        let url = format!("https://dev.azure.com/{org}/{project}/_apis/git/pullrequests");
        let top_s = top.to_string();
        let mut q: Vec<(&str, &str)> = vec![
            ("api-version", API_VERSION),
            ("$top", top_s.as_str()),
            (who_param, user_id),
        ];
        if status != "all" {
            q.push(("searchCriteria.status", status));
        }
        self.get_prs(&url, &q).await
    }

    async fn get_prs(&self, url: &str, query: &[(&str, &str)]) -> Result<Vec<PullRequest>> {
        let resp = self
            .http
            .get(url)
            .query(query)
            .header("Authorization", &self.basic_auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Azure DevOps PR request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Azure DevOps PR list failed: {status}: {text}"));
        }
        let pr: PullRequestResponse =
            resp.json().await.context("parsing PR response")?;
        Ok(pr.value)
    }

    /// Project-scoped build list. Optional narrowers — repo
    /// (`repository.name`), branch, definition ID. `branch_or_ref`
    /// accepts both `main` and `refs/heads/main`; we normalize to
    /// the full ref before querying.
    pub async fn builds(
        &self,
        org: &str,
        project: &str,
        repo: Option<&str>,
        branch: Option<&str>,
        definition_id: Option<i64>,
        top: u32,
    ) -> Result<Vec<Build>> {
        let url = format!("https://dev.azure.com/{org}/{project}/_apis/build/builds");
        let top_s = top.to_string();
        let mut q: Vec<(String, String)> = Vec::new();
        q.push(("api-version".into(), API_VERSION.into()));
        q.push(("$top".into(), top_s));
        q.push(("queryOrder".into(), "queueTimeDescending".into()));
        if let Some(b) = branch {
            let full = if b.starts_with("refs/") {
                b.to_string()
            } else {
                format!("refs/heads/{b}")
            };
            q.push(("branchName".into(), full));
        }
        if let Some(r) = repo {
            q.push(("repositoryName".into(), r.into()));
            q.push(("repositoryType".into(), "TfsGit".into()));
        }
        if let Some(d) = definition_id {
            q.push(("definitions".into(), d.to_string()));
        }
        let q_str: Vec<(&str, &str)> =
            q.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).collect();
        let resp = self
            .http
            .get(&url)
            .query(&q_str)
            .header("Authorization", &self.basic_auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Azure DevOps build request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Azure DevOps build list failed: {status}: {text}"));
        }
        let br: BuildResponse =
            resp.json().await.context("parsing build response")?;
        Ok(br.value)
    }

    /// Resolves the current user's GUID for `mode = mine` /
    /// `reviewing` tabs. Hits `/_apis/connectionData`, which is
    /// org-scoped (any project works).
    pub async fn connection_data(&self, org: &str) -> Result<ConnectionData> {
        let url = format!("https://dev.azure.com/{org}/_apis/connectionData");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.basic_auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Azure DevOps connectionData request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("connectionData failed: {status}: {text}"));
        }
        let cd: ConnectionData = resp.json().await.context("parsing connectionData")?;
        Ok(cd)
    }
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    value: Vec<PullRequest>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct PullRequest {
    #[serde(rename = "pullRequestId")]
    pub id: i64,
    pub title: String,
    pub status: String,
    #[serde(rename = "createdBy")]
    pub created_by: Option<Identity>,
    #[serde(rename = "sourceRefName")]
    pub source_ref: Option<String>,
    #[serde(rename = "targetRefName")]
    pub target_ref: Option<String>,
    #[serde(rename = "creationDate")]
    pub creation_date: Option<String>,
    pub repository: Option<RepoRef>,
    /// Web URL has to be reconstructed — the API returns `url` as
    /// the REST handle, not the human page. We do that in
    /// [`PullRequest::web_url`].
    pub url: Option<String>,
    #[serde(default)]
    pub reviewers: Vec<Reviewer>,
}

impl PullRequest {
    pub fn web_url(&self, org: &str, project: &str) -> String {
        let repo = self.repository.as_ref().map(|r| r.name.as_str()).unwrap_or("repo");
        format!("https://dev.azure.com/{org}/{project}/_git/{repo}/pullrequest/{}", self.id)
    }
    pub fn source_branch_short(&self) -> String {
        short_ref(self.source_ref.as_deref().unwrap_or(""))
    }
    pub fn target_branch_short(&self) -> String {
        short_ref(self.target_ref.as_deref().unwrap_or(""))
    }
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Identity {
    #[serde(rename = "displayName", default)]
    pub display_name: String,
    #[serde(default)]
    pub id: String,
    #[serde(rename = "uniqueName", default)]
    pub unique_name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct RepoRef {
    pub name: String,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Reviewer {
    #[serde(rename = "displayName", default)]
    pub display_name: String,
    /// Vote: 10 = approved, 5 = approved-with-suggestions,
    /// 0 = no vote, -5 = waiting, -10 = rejected.
    #[serde(default)]
    pub vote: i32,
    #[serde(default, rename = "isRequired")]
    pub is_required: bool,
}

#[derive(Debug, Deserialize)]
struct BuildResponse {
    value: Vec<Build>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Build {
    pub id: i64,
    #[serde(rename = "buildNumber")]
    pub build_number: String,
    /// `inProgress`, `completed`, `cancelling`, `postponed`, `notStarted`, `all`.
    pub status: String,
    /// `succeeded`, `failed`, `canceled`, `partiallySucceeded`, `none`.
    /// Set once `status = completed`.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(rename = "sourceBranch")]
    pub source_branch: Option<String>,
    #[serde(rename = "queueTime")]
    pub queue_time: Option<String>,
    #[serde(rename = "startTime")]
    pub start_time: Option<String>,
    #[serde(rename = "finishTime")]
    pub finish_time: Option<String>,
    pub definition: Option<BuildDefinitionRef>,
    pub repository: Option<BuildRepoRef>,
    #[serde(rename = "requestedFor", default)]
    pub requested_for: Option<Identity>,
    #[serde(rename = "_links", default)]
    pub links: Option<BuildLinks>,
}

impl Build {
    /// Short status chip for the conclusion column.
    pub fn status_chip(&self) -> &str {
        match self.status.as_str() {
            "completed" => self.result.as_deref().unwrap_or("done"),
            "inProgress" => "running",
            "notStarted" => "queued",
            "cancelling" => "cancelling",
            "postponed" => "postponed",
            s => s,
        }
    }
    pub fn web_url(&self) -> Option<&str> {
        self.links.as_ref()?.web.as_ref().map(|w| w.href.as_str())
    }
    pub fn source_branch_short(&self) -> String {
        short_ref(self.source_branch.as_deref().unwrap_or(""))
    }
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct BuildDefinitionRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct BuildRepoRef {
    #[serde(default)]
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct BuildLinks {
    pub web: Option<LinkRef>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct LinkRef {
    pub href: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct ConnectionData {
    #[serde(rename = "authenticatedUser")]
    pub authenticated_user: AuthenticatedUser,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct AuthenticatedUser {
    pub id: String,
    #[serde(rename = "providerDisplayName", default)]
    pub provider_display_name: String,
}

fn short_ref(s: &str) -> String {
    s.strip_prefix("refs/heads/")
        .unwrap_or_else(|| s.strip_prefix("refs/tags/").unwrap_or(s))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ref_strips_refs_heads() {
        assert_eq!(short_ref("refs/heads/main"), "main");
        assert_eq!(short_ref("refs/tags/v1"), "v1");
        assert_eq!(short_ref("main"), "main");
        assert_eq!(short_ref(""), "");
    }

    #[test]
    fn build_status_chip_uses_result_when_completed() {
        let b = Build {
            id: 1,
            build_number: "20260605.1".into(),
            status: "completed".into(),
            result: Some("succeeded".into()),
            source_branch: None,
            queue_time: None,
            start_time: None,
            finish_time: None,
            definition: None,
            repository: None,
            requested_for: None,
            links: None,
        };
        assert_eq!(b.status_chip(), "succeeded");
    }

    #[test]
    fn build_status_chip_running_when_in_progress() {
        let b = Build {
            id: 1,
            build_number: "x".into(),
            status: "inProgress".into(),
            result: None,
            source_branch: None,
            queue_time: None,
            start_time: None,
            finish_time: None,
            definition: None,
            repository: None,
            requested_for: None,
            links: None,
        };
        assert_eq!(b.status_chip(), "running");
    }

    #[test]
    fn pr_web_url_uses_org_project_and_repo() {
        let pr = PullRequest {
            id: 42,
            title: "t".into(),
            status: "active".into(),
            created_by: None,
            source_ref: Some("refs/heads/feature".into()),
            target_ref: Some("refs/heads/main".into()),
            creation_date: None,
            repository: Some(RepoRef {
                name: "myrepo".into(),
                id: "".into(),
            }),
            url: None,
            reviewers: vec![],
        };
        assert_eq!(
            pr.web_url("orgx", "proj"),
            "https://dev.azure.com/orgx/proj/_git/myrepo/pullrequest/42"
        );
        assert_eq!(pr.source_branch_short(), "feature");
        assert_eq!(pr.target_branch_short(), "main");
    }
}
