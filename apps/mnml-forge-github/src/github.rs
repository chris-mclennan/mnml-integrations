//! Minimal GitHub REST API client.
//!
//! Endpoints wired:
//!   - `GET /search/issues` — covers issues AND PRs via `is:pr`.
//!   - `GET /repos/{o}/{r}/actions/runs` — Actions workflow runs.
//!   - `GET /user` — auth probe + resolve viewer login.
//!   - `GET /users/{u}/repos`, `GET /orgs/{o}/repos` — repo enumeration
//!     for the workspace-wide tabs (workspace-tabs 2026-08-22).
//!   - `GET /repos/{o}/{r}/pulls?state=…` — per-repo PR list (fan-out
//!     by the workspace_open_prs / workspace_merged_prs tabs).
//!
//! Auth is the same `Authorization: Bearer <token>` for every call.

use anyhow::{Context, Result, anyhow};
use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;

const SEARCH_ENDPOINT: &str = "https://api.github.com/search/issues";

/// GitHub authenticated rate limit is 5000 req/hr — but secondary
/// rate limits kick in on bursty concurrency. Cap parallelism at 4
/// per fan-out to stay a good citizen and survive without 429s on
/// medium-sized orgs. Matches mnml-forge-bitbucket's concurrency cap.
const FANOUT_CONCURRENCY: usize = 4;

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    token: String,
}

impl Client {
    pub fn new(token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mnml-forge-github/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            token: token.to_string(),
        })
    }

    fn auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// #1103 f/u7 (2026-08-20) — verify the current auth token by
    /// calling `GET /user`. Returns the authenticated user's login.
    pub async fn whoami(&self) -> Result<String> {
        let resp = self
            .auth_headers(self.http.get("https://api.github.com/user"))
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
    /// results (the API caps this at 100). First page only.
    pub async fn search(&self, query: &str, per_page: u32) -> Result<Vec<Issue>> {
        let resp = self
            .auth_headers(self.http.get(SEARCH_ENDPOINT))
            .query(&[
                ("q", query),
                ("per_page", &per_page.to_string()),
                ("sort", "updated"),
                ("order", "desc"),
            ])
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
    /// branch. Returns up to `per_page` runs, created_at desc.
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
            .auth_headers(self.http.get(&url))
            .query(&query)
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

    /// Enumerate an owner's repos with their `pushed_at` activity
    /// timestamp. Tries `/users/{owner}/repos` first (works for
    /// personal accounts and lists public repos of orgs); on any
    /// non-2xx falls back to `/orgs/{owner}/repos`. Paginates
    /// forward via the `Link: <…>; rel="next"` header.
    ///
    /// Returns entries sorted by `pushed_at` descending (matches
    /// GitHub's default `sort=pushed&direction=desc`).
    pub async fn list_owner_repos_with_activity(&self, owner: &str) -> Result<Vec<RepoActivity>> {
        // Try user-scope first (works for orgs' public repos too).
        match self
            .paginate_repos(&format!(
                "https://api.github.com/users/{owner}/repos?per_page=100&sort=pushed&type=owner"
            ))
            .await
        {
            Ok(v) if !v.is_empty() => Ok(v),
            Ok(_) | Err(_) => {
                // Fall back to orgs endpoint — required for private
                // org repos when the token has access.
                self.paginate_repos(&format!(
                    "https://api.github.com/orgs/{owner}/repos?per_page=100&sort=pushed"
                ))
                .await
            }
        }
    }

    async fn paginate_repos(&self, initial_url: &str) -> Result<Vec<RepoActivity>> {
        let mut out: Vec<RepoActivity> = Vec::new();
        let mut url: Option<String> = Some(initial_url.to_string());
        // Bounded pagination — 10 pages of 100 == 1000 repos, more
        // than enough for any real workspace. Prevents infinite
        // walks on a mis-parsed Link header.
        let mut pages = 0usize;
        while let Some(next) = url.take()
            && pages < 10
        {
            pages += 1;
            let resp = self
                .auth_headers(self.http.get(&next))
                .send()
                .await
                .with_context(|| format!("GET {next}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("GitHub repos failed: {status}: {body}"));
            }
            let link_header = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let page: Vec<RepoActivity> = resp
                .json()
                .await
                .with_context(|| format!("parsing repos page {pages}"))?;
            out.extend(page);
            url = link_header.as_deref().and_then(parse_link_next);
        }
        Ok(out)
    }

    /// Fan-out — for each `owner/repo` slug, fetch the OPEN PRs and
    /// group them into `RepoPrs`. Concurrency capped at
    /// `FANOUT_CONCURRENCY`. Never errors overall — a per-repo
    /// failure surfaces as `RepoPrs.error` so the tab still renders.
    pub async fn list_repos_open_prs(
        &self,
        owner: &str,
        repos: &[String],
        per_page: u32,
    ) -> Vec<RepoPrs> {
        self.fanout_prs(owner, repos, "open", per_page).await
    }

    /// Fan-out — CLOSED PRs. GitHub returns both closed-without-merge
    /// and merged in `state=closed`; the caller filters to
    /// `merged_at.is_some()` when it wants only merged.
    pub async fn list_repos_closed_prs(
        &self,
        owner: &str,
        repos: &[String],
        per_page: u32,
    ) -> Vec<RepoPrs> {
        self.fanout_prs(owner, repos, "closed", per_page).await
    }

    /// Fan-out — one Actions workflow run row per repo (the most
    /// recent). We fetch a small page and keep the first entry;
    /// `RepoActions.runs` is left as a Vec so future work can grow
    /// into multi-run drill-down without changing the shape.
    pub async fn list_repos_actions(
        &self,
        owner: &str,
        repos: &[String],
        per_repo: u32,
    ) -> Vec<RepoActions> {
        let sem = Arc::new(Semaphore::new(FANOUT_CONCURRENCY));
        let mut futures = FuturesUnordered::new();
        for slug in repos {
            let sem = Arc::clone(&sem);
            let (repo_owner, repo_name) = split_slug_owned(slug, owner);
            let http = self.http.clone();
            let token = self.token.clone();
            let display_slug = slug.clone();
            futures.push(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore not closed");
                let url = format!(
                    "https://api.github.com/repos/{repo_owner}/{repo_name}/actions/runs?per_page={per_repo}"
                );
                let req = http
                    .get(&url)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28");
                match req.send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<ActionsResponse>().await {
                            Ok(body) => RepoActions {
                                slug: display_slug,
                                runs: body.workflow_runs,
                                error: None,
                            },
                            Err(e) => RepoActions {
                                slug: display_slug,
                                runs: Vec::new(),
                                error: Some(format!("parse: {e}")),
                            },
                        }
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        RepoActions {
                            slug: display_slug,
                            runs: Vec::new(),
                            error: Some(short_status(status)),
                        }
                    }
                    Err(e) => RepoActions {
                        slug: display_slug,
                        runs: Vec::new(),
                        error: Some(format!("net: {e}")),
                    },
                }
            });
        }
        let mut out = Vec::with_capacity(repos.len());
        while let Some(row) = futures.next().await {
            out.push(row);
        }
        // Preserve caller-provided slug order (the fan-out completes
        // out of order but we want the display order stable).
        out.sort_by_key(|r| {
            repos
                .iter()
                .position(|s| s == &r.slug)
                .unwrap_or(usize::MAX)
        });
        out
    }

    async fn fanout_prs(
        &self,
        owner: &str,
        repos: &[String],
        state: &str,
        per_page: u32,
    ) -> Vec<RepoPrs> {
        let sem = Arc::new(Semaphore::new(FANOUT_CONCURRENCY));
        let mut futures = FuturesUnordered::new();
        for slug in repos {
            let sem = Arc::clone(&sem);
            let (repo_owner, repo_name) = split_slug_owned(slug, owner);
            let http = self.http.clone();
            let token = self.token.clone();
            let state = state.to_string();
            let display_slug = slug.clone();
            futures.push(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore not closed");
                let url = format!(
                    "https://api.github.com/repos/{repo_owner}/{repo_name}/pulls?state={state}&per_page={per_page}&sort=updated&direction=desc"
                );
                let req = http
                    .get(&url)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28");
                match req.send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<Vec<PullRequest>>().await {
                            Ok(prs) => RepoPrs {
                                slug: display_slug,
                                prs,
                                error: None,
                            },
                            Err(e) => RepoPrs {
                                slug: display_slug,
                                prs: Vec::new(),
                                error: Some(format!("parse: {e}")),
                            },
                        }
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        RepoPrs {
                            slug: display_slug,
                            prs: Vec::new(),
                            error: Some(short_status(status)),
                        }
                    }
                    Err(e) => RepoPrs {
                        slug: display_slug,
                        prs: Vec::new(),
                        error: Some(format!("net: {e}")),
                    },
                }
            });
        }
        let mut out = Vec::with_capacity(repos.len());
        while let Some(row) = futures.next().await {
            out.push(row);
        }
        out.sort_by_key(|r| {
            repos
                .iter()
                .position(|s| s == &r.slug)
                .unwrap_or(usize::MAX)
        });
        out
    }
}

/// Split a config slug into `(owner, repo)`. A slug containing `/`
/// stays split as-is; a bare `repo` name resolves against the
/// default owner. Used by every fan-out helper so the same slug
/// (bare or fully-qualified) can appear in `repos = [...]`,
/// `explicit_repos`, or an `--repos` allowlist without surprise.
fn split_slug<'a>(slug: &'a str, default_owner: &'a str) -> (&'a str, &'a str) {
    match slug.split_once('/') {
        Some((o, r)) if !o.is_empty() && !r.is_empty() => (o, r),
        _ => (default_owner, slug),
    }
}

/// Owned variant of [`split_slug`]. The fan-out closures need owned
/// strings because the borrowed default_owner would not outlive the
/// spawned future.
fn split_slug_owned(slug: &str, default_owner: &str) -> (String, String) {
    let (o, r) = split_slug(slug, default_owner);
    (o.to_string(), r.to_string())
}

/// Short human-readable label for an HTTP status — surfaces as the
/// per-repo error chip in the tree. Matches the tone of
/// mnml-forge-bitbucket's per-repo failure surfacing.
fn short_status(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "auth failed".to_string(),
        404 => "not found".to_string(),
        429 => "rate-limited".to_string(),
        s => format!("HTTP {s}"),
    }
}

/// Parse the `rel="next"` URL out of an RFC 5988 Link header. GitHub
/// returns `<url>; rel="next", <url>; rel="last"` — we only care
/// about `next` for forward pagination. Returns None when the
/// header is absent, malformed, or has no `next` entry (i.e. we're
/// on the last page).
fn parse_link_next(header: &str) -> Option<String> {
    for chunk in header.split(',') {
        let chunk = chunk.trim();
        // Shape: `<https://...>; rel="next"`
        let (url_part, rel_part) = chunk.split_once(';')?;
        let url = url_part
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if rel_part.trim().contains("rel=\"next\"") {
            return Some(url.to_string());
        }
    }
    None
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
    #[serde(default)]
    pub repository_url: Option<String>,
}

impl Issue {
    pub fn is_pr(&self) -> bool {
        self.pull_request.is_some()
    }

    pub fn repo_short(&self) -> String {
        if let Some(url) = &self.repository_url
            && let Some(idx) = url.find("/repos/")
        {
            return url[idx + 7..].to_string();
        }
        let parts: Vec<&str> = self.html_url.split('/').collect();
        if parts.len() >= 5 {
            format!("{}/{}", parts[3], parts[4])
        } else {
            String::new()
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Label {
    pub name: String,
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
#[derive(Debug, Deserialize, Serialize, Clone)]
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

/// Repo activity metadata used by the "recent" scope filter. `pushed_at`
/// is GitHub's canonical "last commit landed" timestamp; `updated_at`
/// covers non-code activity (issues, releases, PR reviews).
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct RepoActivity {
    /// The repo's short name (part after `owner/`). GitHub returns
    /// this as `name`; we keep the field named `name` so it deserializes
    /// straight from the API response.
    pub name: String,
    /// Fully qualified `owner/repo`. Used as the slug in `RepoPrs` /
    /// `RepoActions` since a workspace tab may pull from multiple
    /// owners via the `repos` allowlist.
    pub full_name: String,
    #[serde(default)]
    pub pushed_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Archived repos are excluded from every workspace tab.
    #[serde(default)]
    pub archived: bool,
}

/// One repo's OPEN or CLOSED PRs, kept together for the per-repo tree
/// view. Matches mnml-forge-bitbucket's `RepoPrs` shape (minus
/// `fallback_merged`, which was a v2 nicety not ported here).
#[derive(Debug, Clone)]
pub struct RepoPrs {
    pub slug: String,
    pub prs: Vec<PullRequest>,
    /// Populated when the per-repo fetch failed. Short human-readable
    /// label — `"auth failed"`, `"429"`, `"parse: …"`.
    pub error: Option<String>,
}

/// One repo's recent Actions workflow runs. Sibling of `RepoPrs` for
/// the `workspace_actions` tab.
#[derive(Debug, Clone)]
pub struct RepoActions {
    pub slug: String,
    pub runs: Vec<WorkflowRun>,
    pub error: Option<String>,
}

/// A GitHub pull request as returned by
/// `GET /repos/{owner}/{repo}/pulls`. Fields kept minimal — we only
/// need what the row renders and what `html_url` needs to be present.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[allow(dead_code)]
pub struct PullRequest {
    pub number: i64,
    pub title: String,
    pub html_url: String,
    /// `open` or `closed`. GitHub doesn't distinguish "merged" as a
    /// state; the caller checks `merged_at.is_some()`.
    pub state: String,
    /// True while the PR is a draft. `draft=true` still reports
    /// `state=open`.
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub head: Option<PrRef>,
    #[serde(default)]
    pub base: Option<PrRef>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub merged_at: Option<String>,
}

impl PullRequest {
    pub fn state_chip(&self) -> &'static str {
        if self.merged_at.is_some() {
            "merged"
        } else if self.state == "closed" {
            "closed"
        } else if self.draft {
            "draft"
        } else {
            "open"
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[allow(dead_code)]
pub struct PrRef {
    /// Branch name (`main`, `feature/foo`).
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: Option<String>,
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
            html_url: String::new(),
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
    fn split_slug_fully_qualified() {
        assert_eq!(split_slug("acme/tool", "def"), ("acme", "tool"));
    }

    #[test]
    fn split_slug_bare_resolves_to_default_owner() {
        assert_eq!(split_slug("tool", "acme"), ("acme", "tool"));
    }

    #[test]
    fn split_slug_empty_owner_falls_back_to_default() {
        // A leading slash edge case — treat it as a bare name.
        assert_eq!(split_slug("/tool", "acme"), ("acme", "/tool"));
    }

    #[test]
    fn parse_link_next_returns_next_url() {
        let hdr = r#"<https://api.github.com/orgs/x/repos?page=2>; rel="next", <https://api.github.com/orgs/x/repos?page=5>; rel="last""#;
        assert_eq!(
            parse_link_next(hdr).as_deref(),
            Some("https://api.github.com/orgs/x/repos?page=2")
        );
    }

    #[test]
    fn parse_link_next_none_when_only_last() {
        let hdr = r#"<https://api.github.com/orgs/x/repos?page=5>; rel="last""#;
        assert!(parse_link_next(hdr).is_none());
    }

    #[test]
    fn pr_state_chip_reports_merged_over_closed() {
        let p = PullRequest {
            number: 1,
            title: "t".into(),
            html_url: String::new(),
            state: "closed".into(),
            draft: false,
            user: None,
            head: None,
            base: None,
            updated_at: None,
            merged_at: Some("2026-01-01T00:00:00Z".into()),
        };
        assert_eq!(p.state_chip(), "merged");
    }

    #[test]
    fn pr_state_chip_reports_draft() {
        let p = PullRequest {
            number: 1,
            title: "t".into(),
            html_url: String::new(),
            state: "open".into(),
            draft: true,
            user: None,
            head: None,
            base: None,
            updated_at: None,
            merged_at: None,
        };
        assert_eq!(p.state_chip(), "draft");
    }
}
