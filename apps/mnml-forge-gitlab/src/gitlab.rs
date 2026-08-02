//! Minimal GitLab REST v4 client. Three endpoints are wired:
//!
//!   - Merge requests (per-project or `/merge_requests` scope-spanning).
//!     <https://docs.gitlab.com/ee/api/merge_requests.html>
//!   - Pipelines (per-project, with optional ref).
//!     <https://docs.gitlab.com/ee/api/pipelines.html>
//!   - `/user` (resolves the current user's `id` for
//!     `mode = mine` / `reviewing` tabs).
//!     <https://docs.gitlab.com/ee/api/users.html#list-current-user>
//!
//! Auth: `Authorization: Bearer <PAT>` (the PAT-Header form;
//! GitLab also accepts `PRIVATE-TOKEN: <PAT>` but Bearer is the
//! same across OAuth/PAT and works everywhere).

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    bearer_header: String,
}

impl Client {
    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("mnml-forge-gitlab/0.1.0")
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            bearer_header: format!("Bearer {token}"),
        })
    }

    /// Per-project MR list. `project` is `"group/path"` (URL-encoded
    /// automatically) or a numeric ID.
    pub async fn merge_requests_project(
        &self,
        project: &str,
        state: &str,
        per_page: u32,
    ) -> Result<Vec<MergeRequest>> {
        let pid = urlencoding::encode(project);
        let url = format!("{}/projects/{}/merge_requests", self.base_url, pid);
        let per_s = per_page.to_string();
        let mut q: Vec<(&str, &str)> = vec![
            ("per_page", per_s.as_str()),
            ("order_by", "updated_at"),
            ("sort", "desc"),
        ];
        if state != "all" {
            q.push(("state", state));
        }
        self.get_mrs(&url, &q).await
    }

    /// Instance-wide MR list filtered by author or reviewer.
    /// `who_param` is `"author_id"` or `"reviewer_id"`.
    pub async fn merge_requests_by_person(
        &self,
        who_param: &str,
        user_id: i64,
        state: &str,
        per_page: u32,
    ) -> Result<Vec<MergeRequest>> {
        let url = format!("{}/merge_requests", self.base_url);
        let per_s = per_page.to_string();
        let id_s = user_id.to_string();
        let mut q: Vec<(&str, &str)> = vec![
            ("per_page", per_s.as_str()),
            ("order_by", "updated_at"),
            ("sort", "desc"),
            ("scope", "all"),
            (who_param, id_s.as_str()),
        ];
        if state != "all" {
            q.push(("state", state));
        }
        self.get_mrs(&url, &q).await
    }

    async fn get_mrs(&self, url: &str, query: &[(&str, &str)]) -> Result<Vec<MergeRequest>> {
        let resp = self
            .http
            .get(url)
            .query(query)
            .header("Authorization", &self.bearer_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("GitLab MR request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitLab MR list failed: {status}: {text}"));
        }
        let mrs: Vec<MergeRequest> = resp.json().await.context("parsing MR response")?;
        Ok(mrs)
    }

    /// Per-project pipeline list. `ref_name` optionally narrows to
    /// one branch (None ⇒ all branches).
    pub async fn pipelines(
        &self,
        project: &str,
        ref_name: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<Pipeline>> {
        let pid = urlencoding::encode(project);
        let url = format!("{}/projects/{}/pipelines", self.base_url, pid);
        let per_s = per_page.to_string();
        let mut q: Vec<(&str, &str)> = vec![("per_page", per_s.as_str())];
        if let Some(r) = ref_name {
            q.push(("ref", r));
        }
        let resp = self
            .http
            .get(&url)
            .query(&q)
            .header("Authorization", &self.bearer_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("GitLab pipelines request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitLab pipelines list failed: {status}: {text}"));
        }
        let pipelines: Vec<Pipeline> =
            resp.json().await.context("parsing pipelines response")?;
        Ok(pipelines)
    }

    /// Resolves the current user's ID for `mode = mine / reviewing`
    /// tabs. Hits `/user`.
    pub async fn whoami(&self) -> Result<User> {
        let url = format!("{}/user", self.base_url);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.bearer_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("GitLab /user request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitLab /user failed: {status}: {text}"));
        }
        let u: User = resp.json().await.context("parsing /user response")?;
        Ok(u)
    }
}

// Tiny URL-encoder for project paths — `group/project` → `group%2Fproject`.
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 3);
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char);
                }
                _ => {
                    out.push('%');
                    out.push_str(&format!("{b:02X}"));
                }
            }
        }
        out
    }
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct MergeRequest {
    pub id: i64,
    pub iid: i64,
    pub project_id: i64,
    pub title: String,
    /// `opened`, `closed`, `merged`, `locked`.
    pub state: String,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
    pub author: Option<UserBrief>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub web_url: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub references: Option<MrReferences>,
}

impl MergeRequest {
    /// Short `group/project` derived from `web_url`. The MR API
    /// doesn't return the project path directly, so we recover it
    /// from the URL (which is always `<base>/<group>/<project>/-/merge_requests/<iid>`).
    pub fn project_path_from_url(&self) -> String {
        let after_scheme = self
            .web_url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.web_url);
        // Strip the host segment.
        let after_host = after_scheme
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(after_scheme);
        if let Some(idx) = after_host.find("/-/merge_requests/") {
            return after_host[..idx].to_string();
        }
        String::new()
    }
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct MrReferences {
    pub short: String,
    pub full: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Pipeline {
    pub id: i64,
    pub iid: Option<i64>,
    pub project_id: i64,
    pub sha: String,
    pub r#ref: Option<String>,
    /// `created`, `waiting_for_resource`, `preparing`, `pending`,
    /// `running`, `success`, `failed`, `canceled`, `skipped`,
    /// `manual`, `scheduled`.
    pub status: String,
    pub source: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub web_url: String,
}

impl Pipeline {
    pub fn status_chip(&self) -> &str {
        self.status.as_str()
    }
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct UserBrief {
    pub id: i64,
    pub username: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_path_extracts_group_project_from_web_url() {
        let mr = MergeRequest {
            id: 1,
            iid: 42,
            project_id: 7,
            title: "t".into(),
            state: "opened".into(),
            source_branch: None,
            target_branch: None,
            author: None,
            created_at: None,
            updated_at: None,
            web_url: "https://gitlab.com/exampleorg/myproject/-/merge_requests/42".into(),
            draft: false,
            references: None,
        };
        assert_eq!(mr.project_path_from_url(), "exampleorg/myproject");
    }

    #[test]
    fn project_path_empty_when_url_unparseable() {
        let mr = MergeRequest {
            id: 1,
            iid: 1,
            project_id: 1,
            title: "t".into(),
            state: "opened".into(),
            source_branch: None,
            target_branch: None,
            author: None,
            created_at: None,
            updated_at: None,
            web_url: "https://gitlab.com/not-a-real-mr".into(),
            draft: false,
            references: None,
        };
        assert_eq!(mr.project_path_from_url(), "");
    }

    #[test]
    fn urlencoding_encodes_slashes_to_percent_2f() {
        assert_eq!(urlencoding::encode("group/project"), "group%2Fproject");
        assert_eq!(urlencoding::encode("plain"), "plain");
        assert_eq!(urlencoding::encode("a b"), "a%20b");
    }
}
