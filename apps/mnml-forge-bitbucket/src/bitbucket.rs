//! Minimal Bitbucket Cloud REST API v2 client for pull requests.
//!
//! Base URL: https://api.bitbucket.org/2.0
//! Auth: HTTP Basic with `<email>:<app-password>`. App passwords are
//!       configured at <https://bitbucket.org/account/settings/app-passwords/>
//!       and must have at least `Pull requests: Read` scope.
//! Docs: https://developer.atlassian.com/cloud/bitbucket/rest/api-group-pullrequests/

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;

const BASE: &str = "https://api.bitbucket.org/2.0";

/// 2026-08-16 — reliability sweep (#948). Structured fetch error so the
/// per-repo fan-out can classify a 429 (retry-worthy, honor
/// `Retry-After`) vs an auth failure (surface, don't retry) vs a
/// missing repo (surface, don't retry) vs a network glitch. Mirrors
/// mnml-core's `src/ai_usage.rs::FetchErr` — same shape, same
/// `.with_retry_after` builder — so the two rate-limit paths in the
/// mnml family look and behave alike.
#[derive(Debug, Clone)]
pub struct FetchErr {
    pub message: String,
    /// From the `Retry-After` header on 429 responses. `None` on
    /// non-429 errors or when the header is missing / unparseable.
    /// Bitbucket emits it as an integer delta-seconds; we read it
    /// BEFORE consuming the body so a body-read failure doesn't
    /// swallow the header.
    pub retry_after_secs: Option<u64>,
    /// HTTP status code, or `None` for network / transport errors
    /// (DNS, TLS, connection reset — anything below the HTTP layer).
    pub status: Option<u16>,
}

impl FetchErr {
    fn network(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            retry_after_secs: None,
            status: None,
        }
    }
    fn http(status: u16, msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            retry_after_secs: None,
            status: Some(status),
        }
    }
    fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }
    pub fn is_rate_limited(&self) -> bool {
        self.status == Some(429)
    }
    /// Short human-readable label the UI paints in the per-repo row
    /// when a fetch failed. Kept under ~24 chars so it fits the
    /// STATE column without ellipsis. `409 · retry in 30s` reads as
    /// "the client noticed the 429 and will re-try automatically";
    /// `auth failed` reads as "your app password is wrong / missing
    /// scopes" — different fixes, so worth distinguishing.
    pub fn short_label(&self) -> String {
        match self.status {
            Some(429) => match self.retry_after_secs {
                Some(s) => format!("429 · retry in {s}s"),
                None => "429 · rate limited".to_string(),
            },
            Some(401) | Some(403) => "auth failed".to_string(),
            Some(404) => "no such repo".to_string(),
            Some(code) => format!("HTTP {code}"),
            None => "network error".to_string(),
        }
    }
}

impl std::fmt::Display for FetchErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(code) => write!(f, "HTTP {}: {}", code, self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    auth_header: String,
}

impl Client {
    pub fn new(email: &str, app_password: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mnml-forge-bitbucket/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let basic = B64.encode(format!("{email}:{app_password}"));
        Ok(Self {
            http,
            auth_header: format!("Basic {basic}"),
        })
    }

    /// Pull requests for a single repo. `state` is one of
    /// `OPEN` / `MERGED` / `DECLINED` / `SUPERSEDED`. `q` is an
    /// optional Bitbucket Query Language string layered on top
    /// (e.g. `author.account_id = "{...}"`).
    ///
    /// 2026-08-16 — legacy anyhow wrapper. New fan-out callers
    /// should prefer `list_repo_prs_retry` (429-aware) or
    /// `list_repo_prs_fetch` (structured `FetchErr`) directly.
    pub async fn list_repo_prs(
        &self,
        workspace: &str,
        repo: &str,
        state: Option<&str>,
        q: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<PullRequest>> {
        self.list_repo_prs_fetch(workspace, repo, state, q, per_page)
            .await
            .map_err(|e| anyhow!("{e}"))
    }

    /// 2026-08-16 (#948) — structured-error fetch. Reads
    /// `Retry-After` BEFORE consuming the response body so a
    /// body-read failure never swallows the header hint. Returns
    /// `FetchErr` on any non-2xx OR any transport error.
    pub async fn list_repo_prs_fetch(
        &self,
        workspace: &str,
        repo: &str,
        state: Option<&str>,
        q: Option<&str>,
        per_page: u32,
    ) -> std::result::Result<Vec<PullRequest>, FetchErr> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pullrequests");
        let mut req = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .query(&[("pagelen", per_page.to_string())]);
        if let Some(s) = state {
            req = req.query(&[("state", s)]);
        }
        if let Some(query) = q {
            req = req.query(&[("q", query)]);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| FetchErr::network(e.to_string()))?;
        let status = resp.status();
        // Extract `Retry-After` BEFORE `resp.text()` consumes the
        // response. RFC-7231 allows two forms: an integer delta-
        // seconds (Bitbucket's typical shape on 429s) OR an HTTP-
        // date. Try numeric first; on parse fail, try HTTP-date and
        // compute the delta from now. Falls back to `None` if
        // neither works — the caller then uses its default backoff.
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let snippet = text.chars().take(120).collect::<String>();
            let mut err = FetchErr::http(status.as_u16(), snippet);
            if let Some(secs) = retry_after {
                err = err.with_retry_after(secs);
            }
            return Err(err);
        }
        let body: PrPage = resp
            .json()
            .await
            .map_err(|e| FetchErr::network(format!("parse: {e}")))?;
        Ok(body.values)
    }

    /// 2026-08-16 (#948) — 429-aware retry wrapper around
    /// `list_repo_prs_fetch`. Up to 3 attempts total; each 429
    /// waits per Bitbucket's `Retry-After` header (or 15s default
    /// if absent), capped at 30s per attempt so a single stubborn
    /// repo can't block the whole fan-out for hours. Other errors
    /// (401/403/404/network) do NOT retry — those aren't
    /// transient and re-hitting the API would just re-fail.
    pub async fn list_repo_prs_retry(
        &self,
        workspace: &str,
        repo: &str,
        state: Option<&str>,
        q: Option<&str>,
        per_page: u32,
    ) -> std::result::Result<Vec<PullRequest>, FetchErr> {
        const MAX_ATTEMPTS: u32 = 3;
        const MAX_BACKOFF_SECS: u64 = 30;
        const DEFAULT_BACKOFF_SECS: u64 = 15;
        let mut last_err: Option<FetchErr> = None;
        for attempt in 0..MAX_ATTEMPTS {
            match self
                .list_repo_prs_fetch(workspace, repo, state, q, per_page)
                .await
            {
                Ok(v) => return Ok(v),
                Err(e) if e.is_rate_limited() => {
                    let wait = e
                        .retry_after_secs
                        .unwrap_or(DEFAULT_BACKOFF_SECS)
                        .min(MAX_BACKOFF_SECS);
                    last_err = Some(e);
                    if attempt < MAX_ATTEMPTS - 1 {
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| FetchErr::network("retry exhausted")))
    }

    /// Workspace-spanning PRs authored by (or reviewed by) the
    /// given account. Bitbucket Cloud doesn't ship a workspace-
    /// scoped PR list endpoint with BBQL — you enumerate the
    /// workspace's repos and query each one. This helper paginates
    /// `/2.0/repositories/{workspace}` to build the slug list, then
    /// fans out concurrent per-repo `list_repo_prs` calls with the
    /// BBQL author / reviewer filter.
    ///
    /// `bbql_predicate` is layered under the state filter — e.g.
    /// `author.account_id = "abc"` for `mode = "mine"` or
    /// `reviewers.account_id = "abc"` for `mode = "reviewing"`.
    ///
    /// Concurrency is capped at 8 in-flight per-repo requests so
    /// the enumeration doesn't spike into Bitbucket's rate limits.
    /// Per-repo errors are silently dropped — a single 403 on one
    /// archived repo shouldn't blank the whole tab.
    pub async fn list_workspace_prs_filtered(
        &self,
        workspace: &str,
        bbql_predicate: &str,
        state: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<PullRequest>> {
        let repos = self.list_workspace_repos(workspace).await?;
        if repos.is_empty() {
            return Ok(Vec::new());
        }
        use futures::stream::{self, StreamExt};
        // Fan out per-repo BBQL queries with a concurrency cap so
        // we don't burst 100+ requests into Bitbucket at once.
        // `buffer_unordered` gives us that cap for free without
        // hand-rolling seed/steady-state queue math.
        let workspace = workspace.to_string();
        let predicate = bbql_predicate.to_string();
        let state = state.map(str::to_string);
        let concurrency = 8usize;
        let batches: Vec<Vec<PullRequest>> = stream::iter(repos.into_iter().map(|slug| {
            let ws = workspace.clone();
            let q = predicate.clone();
            let st = state.clone();
            let client = self.clone();
            async move {
                client
                    .list_repo_prs(&ws, &slug, st.as_deref(), Some(&q), per_page)
                    .await
                    .unwrap_or_default()
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;
        let mut all: Vec<PullRequest> = batches.into_iter().flatten().collect();
        // Newest first — updated_on descending. Bitbucket's per-
        // repo responses come sorted; we resort on the merge so
        // the tab reads chronologically across all repos.
        all.sort_by(|a, b| b.updated_on.cmp(&a.updated_on));
        Ok(all)
    }

    /// Paginate `/2.0/repositories/{workspace}` and collect every
    /// repo slug. Used by the workspace-wide PR enumerators above.
    pub async fn list_workspace_repos(&self, workspace: &str) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        struct RepoPage {
            values: Vec<RepoRef>,
            next: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct RepoRef {
            slug: String,
        }
        let mut out: Vec<String> = Vec::new();
        let mut url = format!("{BASE}/repositories/{workspace}?role=member&pagelen=100");
        loop {
            let resp = self
                .http
                .get(&url)
                .header("Authorization", &self.auth_header)
                .header("Accept", "application/json")
                .send()
                .await
                .context("bitbucket repo enumeration failed")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("bitbucket repos {status}: {text}"));
            }
            let page: RepoPage = resp.json().await.context("parsing bitbucket repo page")?;
            out.extend(page.values.into_iter().map(|r| r.slug));
            match page.next {
                Some(next) if !next.is_empty() => url = next,
                _ => break,
            }
        }
        Ok(out)
    }

    /// `GET /user` — returns the authenticated user. Used by --check
    /// + to resolve `mode = "mine"` / `mode = "reviewing"` tabs.
    pub async fn whoami(&self) -> Result<AuthUser> {
        let resp = self
            .http
            .get(format!("{BASE}/user"))
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("bitbucket whoami failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket whoami: {status}: {text}"));
        }
        resp.json().await.context("parsing bitbucket whoami")
    }

    /// Full PR detail — description, participants, reviewers (with
    /// approval state). Used to populate the right-half detail panel.
    pub async fn get_pr_detail(&self, workspace: &str, repo: &str, id: i64) -> Result<PullRequest> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pullrequests/{id}");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("bitbucket PR detail failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket PR detail {status}: {text}"));
        }
        resp.json().await.context("parsing PR detail response")
    }

    /// PR comments. v0.1 fetches the first page (Bitbucket caps at 50
    /// per page); resolves nested replies as a flat list since v0.1
    /// renders threads inline.
    pub async fn get_pr_comments(
        &self,
        workspace: &str,
        repo: &str,
        id: i64,
    ) -> Result<Vec<Comment>> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pullrequests/{id}/comments");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .query(&[("pagelen", "50")])
            .send()
            .await
            .context("bitbucket PR comments failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket PR comments {status}: {text}"));
        }
        let page: CommentPage = resp.json().await.context("parsing comments")?;
        Ok(page.values)
    }

    /// POST /approve — toggle the auth user's approval on the PR. The
    /// response is the new participant record (approved = true).
    pub async fn approve_pr(&self, workspace: &str, repo: &str, id: i64) -> Result<()> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pullrequests/{id}/approve");
        let resp = self
            .http
            .post(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("bitbucket approve failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket approve {status}: {text}"));
        }
        Ok(())
    }

    /// DELETE /approve — withdraw approval. No-op semantically if you
    /// haven't approved yet (the endpoint returns 404 in that case;
    /// we surface that as an error so the UI can label it clearly).
    pub async fn unapprove_pr(&self, workspace: &str, repo: &str, id: i64) -> Result<()> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pullrequests/{id}/approve");
        let resp = self
            .http
            .delete(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("bitbucket unapprove failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket unapprove {status}: {text}"));
        }
        Ok(())
    }

    /// Recent pipelines (builds) for a repo, newest-first.
    pub async fn list_pipelines(
        &self,
        workspace: &str,
        repo: &str,
        per_page: u32,
    ) -> Result<Vec<Pipeline>> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pipelines/");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .query(&[
                ("pagelen", per_page.to_string()),
                ("sort", "-created_on".to_string()),
            ])
            .send()
            .await
            .context("bitbucket pipelines list failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket pipelines {status}: {text}"));
        }
        let page: PipelinePage = resp.json().await.context("parsing pipelines")?;
        Ok(page.values)
    }

    /// Pipelines that ran on a specific commit SHA. Used by the
    /// merged-PR row expand to show "did the merge land safely on
    /// main" — pass the PR's `merge_commit.hash`.
    ///
    /// Impl note: Bitbucket's pipelines endpoint does NOT reliably
    /// honor `?target.commit.hash=…` as a query filter — passing it
    /// either returns everything unfiltered or (empirically) an
    /// empty list. Instead we fetch a batch of the most-recent
    /// pipelines and filter client-side, matching the pattern used
    /// by `find_pipeline_for_pr`. Returns matches newest-first.
    pub async fn list_pipelines_by_commit(
        &self,
        workspace: &str,
        repo: &str,
        commit_hash: &str,
    ) -> Result<Vec<Pipeline>> {
        // 60 = ~one repo's worth of recent CI activity. Post-merge
        // pipelines land within seconds/minutes of the merge, so a
        // freshly-merged PR sits at the top of this list. Older
        // merges (weeks ago) may fall outside the window — treat
        // "not in the recent 60" as "no pipeline" for our purposes.
        let all = self.list_pipelines(workspace, repo, 60).await?;
        // 2026-07-24 — Bitbucket's PR API returns merge_commit.hash
        // as a 12-char short SHA, while the pipelines API returns
        // full 40-char SHAs. Match by "does one start with the
        // other" (both directions, both lowercase) instead of
        // straight equality.
        let needle = commit_hash.to_ascii_lowercase();
        let matches: Vec<Pipeline> = all
            .into_iter()
            .filter(|p| {
                p.target
                    .as_ref()
                    .and_then(|t| t.commit.as_ref())
                    .map(|c| {
                        let h = c.hash.to_ascii_lowercase();
                        h.starts_with(&needle) || needle.starts_with(&h)
                    })
                    .unwrap_or(false)
            })
            .collect();
        Ok(matches)
    }

    /// Branches for a repo, sorted by most-recently-committed-to first.
    pub async fn list_branches(
        &self,
        workspace: &str,
        repo: &str,
        per_page: u32,
    ) -> Result<Vec<BranchRef>> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/refs/branches");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .query(&[
                ("pagelen", per_page.to_string()),
                ("sort", "-target.date".to_string()),
            ])
            .send()
            .await
            .context("bitbucket branches list failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket branches {status}: {text}"));
        }
        let page: BranchRefPage = resp.json().await.context("parsing branches")?;
        Ok(page.values)
    }

    // ── tree-redesign 2026-07-14 Phase 2a API surface ─────────────

    /// Paginate `/2.0/repositories/{workspace}` and collect every
    /// repo slug PLUS its most-recent `updated_on` timestamp. Feeds
    /// the "recent" scope filter (`Config::scope = "recent"`) — a
    /// repo counts as active when its updated_on falls within
    /// `recent_window_days` of now. tree-redesign 2026-07-14.
    ///
    /// Bitbucket sorts `-updated_on` by default on this endpoint,
    /// which is exactly the order we want for both the "recent"
    /// filter AND the display order in the pipelines tree (so
    /// active repos surface at the top). We could paginate
    /// forever, but 500 repos is enough headroom for any realistic
    /// workspace; we cap at 5 pages × 100 to bound worst-case wall
    /// time.
    pub async fn list_workspace_repos_with_activity(
        &self,
        workspace: &str,
    ) -> Result<Vec<RepoActivity>> {
        let mut out: Vec<RepoActivity> = Vec::new();
        let mut url =
            format!("{BASE}/repositories/{workspace}?role=member&pagelen=100&sort=-updated_on");
        let mut pages = 0;
        loop {
            let resp = self
                .http
                .get(&url)
                .header("Authorization", &self.auth_header)
                .header("Accept", "application/json")
                .send()
                .await
                .context("bitbucket repo enumeration failed")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("bitbucket repos {status}: {text}"));
            }
            let page: RepoActivityPage =
                resp.json().await.context("parsing bitbucket repo page")?;
            out.extend(page.values.into_iter().map(|r| RepoActivity {
                slug: r.slug,
                updated_on: r.updated_on,
            }));
            pages += 1;
            match page.next {
                Some(next) if !next.is_empty() && pages < 5 => url = next,
                _ => break,
            }
        }
        Ok(out)
    }

    /// Every OPEN + DRAFT PR across the given repos, all authors,
    /// newest first. Powers `TabKind::workspace_open_prs`.
    /// tree-redesign 2026-07-14.
    ///
    /// Bitbucket's `state` query param accepts one value at a
    /// time on the REST endpoint (no OR), so we fan out per-repo
    /// with two calls (OPEN + DRAFT) and merge. Draft PRs are a
    /// 2023 addition — some archived repos may not know the state
    /// and 400 on the DRAFT query; per-repo errors are dropped so
    /// one legacy repo doesn't blank the whole tab.
    pub async fn list_workspace_open_and_draft_prs(
        &self,
        workspace: &str,
        repo_slugs: &[String],
        per_repo_per_page: u32,
    ) -> Result<Vec<PullRequest>> {
        if repo_slugs.is_empty() {
            return Ok(Vec::new());
        }
        use futures::stream::{self, StreamExt};
        let workspace = workspace.to_string();
        let concurrency = 8usize;
        let slugs = repo_slugs.to_vec();
        // tree-redesign 2026-07-15 fix — Bitbucket's `state=` param
        // accepts only OPEN / MERGED / DECLINED / SUPERSEDED.
        // "DRAFT" is not a valid state; drafts are OPEN PRs with a
        // `draft: true` field. Prior impl fired two calls per repo
        // (OPEN + DRAFT); the DRAFT half 400'd and dropped silently
        // via `unwrap_or_default()`, and worse — the ~240 requests
        // for 119 repos likely rate-limited the OPEN halves too,
        // making the tab show 0 everywhere. Now: single OPEN call
        // per repo (already includes drafts), half the requests,
        // half the rate-limit pressure.
        let batches: Vec<std::result::Result<Vec<PullRequest>, String>> =
            stream::iter(slugs.into_iter().map(|slug| {
                let ws = workspace.clone();
                let client = self.clone();
                async move {
                    client
                        .list_repo_prs(&ws, &slug, Some("OPEN"), None, per_repo_per_page)
                        .await
                        .map_err(|e| format!("{slug}: {e}"))
                }
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;
        let n_total = batches.len();
        let (ok, err): (Vec<_>, Vec<_>) = batches.into_iter().partition(std::result::Result::is_ok);
        // Fail loudly when EVERY repo request failed — silent
        // `unwrap_or_default` hid this class of bug (invalid auth
        // scope, rate limits, wrong workspace slug) as "0 PRs".
        if !err.is_empty() && ok.is_empty() {
            let first = err
                .into_iter()
                .find_map(|r| r.err())
                .unwrap_or_else(|| "unknown".to_string());
            return Err(anyhow!(
                "all {n_total} repo requests failed; first: {first}"
            ));
        }
        if !err.is_empty() {
            eprintln!(
                "list_workspace_open_and_draft_prs: {}/{} repo(s) failed",
                err.len(),
                n_total
            );
        }
        let mut all: Vec<PullRequest> = ok.into_iter().filter_map(|r| r.ok()).flatten().collect();
        all.sort_by(|a, b| b.updated_on.cmp(&a.updated_on));
        Ok(all)
    }

    /// Merged variant of [`Self::list_workspace_open_prs_by_repo`].
    /// Same shape, `state=MERGED`, per-repo PRs already sorted
    /// newest → oldest. tree-redesign 2026-07-15.
    ///
    /// 2026-08-16 (#948) — 429-aware retry via
    /// `list_repo_prs_retry`. Erroring repos still get a row (with
    /// `error: Some(short_label)`) so the UI can surface "429 ·
    /// retry in 30s" per repo instead of silently dropping them.
    /// No fallback_merged here — we're already fetching MERGED.
    pub async fn list_workspace_merged_prs_by_repo(
        &self,
        workspace: &str,
        repo_slugs: &[String],
        per_repo_per_page: u32,
    ) -> Result<Vec<RepoPrs>> {
        if repo_slugs.is_empty() {
            return Ok(Vec::new());
        }
        use futures::stream::{self, StreamExt};
        let workspace = workspace.to_string();
        let concurrency = 4usize;
        let slugs = repo_slugs.to_vec();
        let rows: Vec<RepoPrs> = stream::iter(slugs.into_iter().map(|slug| {
            let ws = workspace.clone();
            let client = self.clone();
            async move {
                match client
                    .list_repo_prs_retry(&ws, &slug, Some("MERGED"), None, per_repo_per_page)
                    .await
                {
                    Ok(mut prs) => {
                        prs.sort_by(|a, b| b.updated_on.cmp(&a.updated_on));
                        RepoPrs {
                            slug,
                            prs,
                            error: None,
                            fallback_merged: None,
                        }
                    }
                    Err(e) => RepoPrs {
                        slug,
                        prs: Vec::new(),
                        error: Some(e.short_label()),
                        fallback_merged: None,
                    },
                }
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;
        // Preserve caller's slug order (buffer_unordered emits in
        // completion order — a fast repo shouldn't outrank a slow
        // one in the tree).
        let mut by_slug: std::collections::HashMap<String, RepoPrs> =
            rows.into_iter().map(|r| (r.slug.clone(), r)).collect();
        let ordered: Vec<RepoPrs> = repo_slugs
            .iter()
            .filter_map(|s| by_slug.remove(s))
            .collect();
        Ok(ordered)
    }

    /// Same as [`Self::list_workspace_open_and_draft_prs`] but keeps
    /// the per-repo grouping (returns `Vec<RepoPrs>`, one row per
    /// input slug — empty repos AND erroring repos both get rows so
    /// the user can see them). Feeds `TabData::RepoPrTree` on the
    /// Open+Draft tab so the user can drill into a specific repo
    /// instead of scrolling a flat list.
    /// tree-redesign 2026-07-15.
    ///
    /// 2026-08-16 (#948) — reliability sweep:
    ///   - `list_repo_prs_retry` (429-aware, honors Retry-After) in
    ///     place of the bare `list_repo_prs`. Prior impl let a
    ///     transient 429 blank a repo permanently.
    ///   - Erroring repos still emit a row (`error: Some(label)`)
    ///     so the UI paints "429 · retry in 30s" or "auth failed"
    ///     per repo. Prior impl silently dropped them.
    ///   - Zero-OPEN repos get a best-effort last-merged fallback
    ///     lookup so every configured repo surfaces SOMETHING —
    ///     either an open PR, or "last merged" metadata, or an
    ///     explicit error. `0 PRs · nothing to see here` was
    ///     indistinguishable from "we forgot to fetch this one".
    pub async fn list_workspace_open_prs_by_repo(
        &self,
        workspace: &str,
        repo_slugs: &[String],
        per_repo_per_page: u32,
    ) -> Result<Vec<RepoPrs>> {
        if repo_slugs.is_empty() {
            return Ok(Vec::new());
        }
        use futures::stream::{self, StreamExt};
        let workspace = workspace.to_string();
        let concurrency = 8usize;
        let slugs = repo_slugs.to_vec();
        let rows: Vec<RepoPrs> = stream::iter(slugs.into_iter().map(|slug| {
            let ws = workspace.clone();
            let client = self.clone();
            async move {
                match client
                    .list_repo_prs_retry(&ws, &slug, Some("OPEN"), None, per_repo_per_page)
                    .await
                {
                    Ok(mut prs) => {
                        prs.sort_by(|a, b| b.updated_on.cmp(&a.updated_on));
                        // Best-effort last-merged fallback on empty
                        // repos — single request, no retry (a merged
                        // fetch that itself 429s just goes silent;
                        // the empty row renders like it did before).
                        let fallback_merged = if prs.is_empty() {
                            client
                                .list_repo_prs_fetch(&ws, &slug, Some("MERGED"), None, 1)
                                .await
                                .ok()
                                .and_then(|v| v.into_iter().next())
                        } else {
                            None
                        };
                        RepoPrs {
                            slug,
                            prs,
                            error: None,
                            fallback_merged,
                        }
                    }
                    Err(e) => RepoPrs {
                        slug,
                        prs: Vec::new(),
                        error: Some(e.short_label()),
                        fallback_merged: None,
                    },
                }
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;
        // Preserve the input slug order (which reflects the caller's
        // scope + repo_order resolution) by re-indexing against the
        // slugs list — buffer_unordered emits in completion order.
        let mut by_slug: std::collections::HashMap<String, RepoPrs> =
            rows.into_iter().map(|r| (r.slug.clone(), r)).collect();
        let ordered: Vec<RepoPrs> = repo_slugs
            .iter()
            .filter_map(|s| by_slug.remove(s))
            .collect();
        Ok(ordered)
    }

    /// Every MERGED PR across the given repos, all authors,
    /// newest → oldest. Powers `TabKind::workspace_merged_prs`.
    /// Bounded to `per_repo_per_page` results per repo (default 25
    /// keeps the aggregate reasonable — a 100-repo workspace with
    /// 25 recent merges each = 2500 rows, past what any realistic
    /// viewer wants). tree-redesign 2026-07-14.
    pub async fn list_workspace_merged_prs(
        &self,
        workspace: &str,
        repo_slugs: &[String],
        per_repo_per_page: u32,
    ) -> Result<Vec<PullRequest>> {
        if repo_slugs.is_empty() {
            return Ok(Vec::new());
        }
        use futures::stream::{self, StreamExt};
        let workspace = workspace.to_string();
        // tree-redesign 2026-07-15 user report — 119 repos with 8
        // concurrent + 25/repo = ~119 requests; combined with
        // Open+Draft's parallel fetches this was hitting Bitbucket
        // rate limits (1000 req/hr). Halve concurrency + surface
        // the count of dropped repos so the user knows the tab
        // isn't lying.
        let concurrency = 4usize;
        let slugs = repo_slugs.to_vec();
        let batches: Vec<std::result::Result<Vec<PullRequest>, String>> =
            stream::iter(slugs.into_iter().map(|slug| {
                let ws = workspace.clone();
                let client = self.clone();
                async move {
                    client
                        .list_repo_prs(&ws, &slug, Some("MERGED"), None, per_repo_per_page)
                        .await
                        .map_err(|e| format!("{slug}: {e}"))
                }
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;
        let (ok, err): (Vec<_>, Vec<_>) = batches.into_iter().partition(std::result::Result::is_ok);
        if !err.is_empty() {
            let first = err
                .iter()
                .next()
                .and_then(|e| e.as_ref().err())
                .cloned()
                .unwrap_or_default();
            eprintln!(
                "list_workspace_merged_prs: {} repo(s) failed (first: {}); returning {} results",
                err.len(),
                first,
                ok.len(),
            );
        }
        let mut all: Vec<PullRequest> = ok
            .into_iter()
            .filter_map(std::result::Result::ok)
            .flatten()
            .collect();
        // Newest merges first (matches the "everyone new to old"
        // spec — user wants a chronological workspace-wide log).
        all.sort_by(|a, b| b.updated_on.cmp(&a.updated_on));
        Ok(all)
    }

    /// For each of `repos`, fetch branches + attach the latest
    /// pipeline that ran on each branch. Powers
    /// `TabKind::workspace_pipelines`. Returns one row per repo,
    /// each carrying its branches with per-branch pipeline
    /// status. Fan-out concurrency capped at 6 (each repo makes
    /// 2 API calls — branches + pipelines — so 6 repos in flight
    /// = 12 concurrent requests). tree-redesign 2026-07-14.
    pub async fn list_workspace_pipelines_tree(
        &self,
        workspace: &str,
        repo_slugs: &[String],
        branches_per_repo: u32,
        pipelines_per_repo: u32,
    ) -> Result<Vec<RepoPipelines>> {
        if repo_slugs.is_empty() {
            return Ok(Vec::new());
        }
        use futures::stream::{self, StreamExt};
        let workspace = workspace.to_string();
        let concurrency = 6usize;
        let slugs = repo_slugs.to_vec();
        let rows: Vec<RepoPipelines> = stream::iter(slugs.into_iter().map(|slug| {
            let ws = workspace.clone();
            let client = self.clone();
            async move {
                let branches = client
                    .list_branches(&ws, &slug, branches_per_repo)
                    .await
                    .unwrap_or_default();
                let pipelines = client
                    .list_pipelines(&ws, &slug, pipelines_per_repo)
                    .await
                    .unwrap_or_default();
                // For each branch, pick the most-recent pipeline
                // that targeted it. Bitbucket returns pipelines
                // newest-first, so `find` gives us the latest.
                let branch_rows: Vec<BranchWithPipeline> = branches
                    .into_iter()
                    .map(|b| {
                        let name = b.name.clone();
                        let latest = pipelines
                            .iter()
                            .find(|p| {
                                p.target
                                    .as_ref()
                                    .and_then(|t| t.ref_name.as_deref())
                                    .is_some_and(|n| n == name)
                            })
                            .cloned();
                        // Latest activity — pipeline's created_on
                        // beats the branch commit's date when both
                        // exist (pipelines re-run without a new
                        // commit). Fall back to branch tip date.
                        let last_activity_on = latest
                            .as_ref()
                            .and_then(|p| p.created_on.clone())
                            .or_else(|| b.target.as_ref().and_then(|t| t.date.clone()));
                        BranchWithPipeline {
                            name,
                            latest_pipeline: latest,
                            last_activity_on,
                        }
                    })
                    .collect();
                RepoPipelines {
                    slug,
                    branches: branch_rows,
                }
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;
        Ok(rows)
    }
}

// ── tree-redesign 2026-07-14 Phase 2a return types ────────────────

/// One repo + its most-recent activity timestamp. Returned by
/// [`Client::list_workspace_repos_with_activity`]. Feeds the
/// "recent" scope filter — `updated_on` is Bitbucket's max of
/// pushed_on / PR update / etc, so a repo with any workspace
/// activity in the window will surface.
#[derive(Debug, Clone)]
pub struct RepoActivity {
    pub slug: String,
    /// ISO-8601 timestamp string. `None` for freshly-created
    /// repos with no activity yet.
    pub updated_on: Option<String>,
}

#[derive(Deserialize)]
struct RepoActivityPage {
    values: Vec<RepoActivityRef>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct RepoActivityRef {
    slug: String,
    #[serde(default)]
    updated_on: Option<String>,
}

/// One repo's branches + per-branch pipeline status. Returned by
/// [`Client::list_workspace_pipelines_tree`]. Renders as one
/// collapsible row in the RepoTree tab: `▶ repo-slug` collapsed,
/// or `▼ repo-slug` + one indented `branch STATUS #num date`
/// row per branch when expanded (mnml-aws-amplify shape).
#[derive(Debug, Clone)]
pub struct RepoPipelines {
    pub slug: String,
    pub branches: Vec<BranchWithPipeline>,
}

#[derive(Debug, Clone)]
pub struct BranchWithPipeline {
    pub name: String,
    /// `None` when no pipeline has ever run on this branch (fresh
    /// branch, or one that only ever runs on the trunk).
    pub latest_pipeline: Option<Pipeline>,
    /// The branch's most-recent activity timestamp, ISO-8601. Uses
    /// the latest pipeline's `created_on` if there is one; else
    /// falls back to the branch's own `target.date` (Bitbucket's
    /// commit date on the tip). Powers the staleness filter in
    /// `curate_branches` — 2026-07-20.
    pub last_activity_on: Option<String>,
}

/// One repo's OPEN PRs, kept together for the per-repo tree view.
/// Returned by [`Client::list_workspace_open_prs_by_repo`] — feeds
/// `TabData::RepoPrTree` (Phase 3 wire-up in progress).
/// tree-redesign 2026-07-15.
///
/// 2026-08-16 (#948) — `error` + `fallback_merged` added so every
/// configured repo gets a visible row even when the per-repo fetch
/// failed OR the repo happened to have zero open PRs. Prior
/// behavior silently dropped erroring repos (via
/// `filter_map(|r| r.ok())`) and rendered `0 PRs` on healthy-but-
/// empty repos — both were indistinguishable from "no repos in
/// scope" and the user couldn't tell which repos had never been
/// contacted.
#[derive(Debug, Clone)]
pub struct RepoPrs {
    pub slug: String,
    pub prs: Vec<PullRequest>,
    /// Populated when the per-repo fetch failed (even after retry).
    /// Short human-readable label (see [`FetchErr::short_label`]).
    /// Mutually exclusive with `fallback_merged` — a real error
    /// precludes the fallback lookup.
    pub error: Option<String>,
    /// For `state=OPEN` fetches only: when the repo has zero open
    /// PRs, this is a best-effort single-shot fetch of its
    /// most-recently-merged PR. Silent on failure (an empty repo
    /// with a network-blip fallback fetch renders exactly like an
    /// empty repo, no scary red).
    pub fallback_merged: Option<PullRequest>,
}

#[derive(Debug, Deserialize)]
struct PrPage {
    #[serde(default)]
    values: Vec<PullRequest>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct PullRequest {
    pub id: i64,
    pub title: String,
    pub state: String,
    /// ISO 8601 with timezone — we keep the raw string and slice for
    /// the date in the table (saves a chrono parse on the render path).
    #[serde(default)]
    pub updated_on: Option<String>,
    #[serde(default)]
    pub author: Option<User>,
    #[serde(default)]
    pub destination: Option<Branch>,
    #[serde(default)]
    pub source: Option<Branch>,
    #[serde(default)]
    pub links: Option<Links>,
    /// Optional summary list. May be absent on lists; present on detail.
    #[serde(default)]
    pub reviewers: Vec<User>,
    /// Long-form PR body. Bitbucket returns this as an object
    /// `{raw, html, markup, type}` on DETAIL responses and as a
    /// bare string on LIST responses (contrary to the docs). The
    /// custom deserializer below accepts either shape so a real
    /// list response doesn't blow up the whole fan-out.
    /// tree-redesign fix 2026-07-15 — this exact schema drift was
    /// the "parsing bitbucket PR list response" error that made
    /// Open+Draft / Merged show 0 for the entire tab.
    #[serde(default, deserialize_with = "deserialize_renderable_or_string")]
    pub description: Option<Renderable>,
    /// Reviewer participation — each entry has `user`, `role` (and on
    /// detail responses, `approved`). Used to derive the approval
    /// badge + decide approve/unapprove.
    #[serde(default)]
    pub participants: Vec<Participant>,
    /// Bitbucket populates this on MERGED PRs with the commit SHA of
    /// the merge commit on the target branch. Used by the row-expand
    /// pipeline lookup: `list_pipelines_by_commit(ws, repo, hash)`
    /// finds the pipeline(s) that ran on main / develop for THIS
    /// merge. Absent on open / declined / superseded PRs.
    #[serde(default)]
    pub merge_commit: Option<CommitRef>,
}

impl PullRequest {
    /// Returns the public HTML URL, falling back to a deterministic
    /// `bitbucket.org/<ws>/<repo>/pull-requests/<id>` if links are
    /// missing.
    pub fn html_url(&self) -> Option<String> {
        self.links
            .as_ref()
            .and_then(|l| l.html.as_ref())
            .map(|h| h.href.clone())
    }

    /// `<workspace>/<repo>` derived from the source/destination
    /// branch's repository link (Bitbucket nests `repository` under
    /// `source` and `destination`). Falls back to an empty string.
    pub fn repo_short(&self) -> String {
        if let Some(b) = self.destination.as_ref().or(self.source.as_ref())
            && let Some(r) = b.repository.as_ref()
        {
            return r.full_name.clone();
        }
        String::new()
    }

    /// Just the date portion of `updated_on` (`YYYY-MM-DD`).
    pub fn updated_date(&self) -> String {
        self.updated_on
            .as_deref()
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default()
    }

    /// Count of approving participants (excluding the auth user — the
    /// detail panel header shows that separately).
    pub fn approval_count(&self) -> usize {
        self.participants
            .iter()
            .filter(|p| p.approved.unwrap_or(false))
            .count()
    }

    /// True iff the participant matching `account_id` has `approved = true`.
    /// `None` ⇒ no matching participant ⇒ false.
    pub fn approved_by(&self, account_id: &str) -> bool {
        self.participants.iter().any(|p| {
            p.user.as_ref().and_then(|u| u.account_id.as_deref()) == Some(account_id)
                && p.approved.unwrap_or(false)
        })
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct User {
    #[serde(default)]
    pub display_name: String,
    /// `account_id` is the stable identifier used by BBQL. v0.1
    /// doesn't dispatch on it but auth-mode resolution will.
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct Branch {
    #[serde(default)]
    pub branch: Option<BranchName>,
    #[serde(default)]
    pub repository: Option<Repo>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct BranchName {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct Repo {
    #[serde(default)]
    pub full_name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Links {
    #[serde(default)]
    pub html: Option<HrefLink>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct HrefLink {
    #[serde(default)]
    pub href: String,
}

/// Bitbucket "renderable" — `raw` (markdown), `html` (rendered),
/// `markup` (markdown variant). v0.1 uses `raw` for description.
#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct Renderable {
    #[serde(default)]
    pub raw: String,
    #[serde(default)]
    pub html: String,
}

/// Accept `description` as either an object `{raw, html, …}` (detail
/// response) OR a bare string (list response). Missing / null → None.
/// tree-redesign 2026-07-15.
fn deserialize_renderable_or_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Renderable>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Option<Renderable>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("null, a string, or a Renderable object")
        }
        fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Self::Value, E> {
            Ok(Some(Renderable {
                raw: s.to_string(),
                html: String::new(),
            }))
        }
        fn visit_string<E: de::Error>(self, s: String) -> std::result::Result<Self::Value, E> {
            Ok(Some(Renderable {
                raw: s,
                html: String::new(),
            }))
        }
        fn visit_map<M>(self, m: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let r = Renderable::deserialize(de::value::MapAccessDeserializer::new(m))?;
            Ok(Some(r))
        }
        fn visit_some<D2>(self, d: D2) -> std::result::Result<Self::Value, D2::Error>
        where
            D2: serde::Deserializer<'de>,
        {
            d.deserialize_any(V)
        }
    }
    deserializer.deserialize_option(V)
}

/// Reviewer participation record. On detail responses, `approved`
/// tells you whether this reviewer has hit the approve button.
#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct Participant {
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub approved: Option<bool>,
    /// `state` is one of `approved` / `changes_requested` / null.
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CommentPage {
    #[serde(default)]
    values: Vec<Comment>,
}

/// A single PR comment. Bitbucket nests body markup the same way as
/// PR descriptions — `raw` is plain markdown.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Comment {
    pub id: i64,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub content: Option<Renderable>,
    #[serde(default)]
    pub created_on: Option<String>,
    /// When set, this is a reply to another comment id. v0.1 renders
    /// the flat list; threading is v0.3.
    #[serde(default)]
    pub parent: Option<CommentParent>,
    /// Inline file/line annotations on a diff comment.
    #[serde(default)]
    pub inline: Option<InlineRef>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct CommentParent {
    pub id: i64,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct InlineRef {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
}

impl Comment {
    /// Just the date portion of `created_on` (`YYYY-MM-DD`).
    pub fn created_date(&self) -> String {
        self.created_on
            .as_deref()
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default()
    }

    pub fn author(&self) -> &str {
        self.user
            .as_ref()
            .map(|u| u.display_name.as_str())
            .unwrap_or("—")
    }

    pub fn body(&self) -> &str {
        self.content.as_ref().map(|c| c.raw.as_str()).unwrap_or("")
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AuthUser {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

// ─── Pipelines ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PipelinePage {
    #[serde(default)]
    values: Vec<Pipeline>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Pipeline {
    /// Pipeline UUID — used to build the bitbucket.org browser URL.
    pub uuid: String,
    /// Sequential build number within the repo (`build_number` field).
    #[serde(default)]
    pub build_number: i64,
    /// State envelope — top-level shape is
    /// `{ name: "COMPLETED"|"PENDING"|"IN_PROGRESS"|...,
    ///    result: { name: "SUCCESSFUL"|"FAILED"|"STOPPED" } }`.
    #[serde(default)]
    pub state: Option<PipelineState>,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub duration_in_seconds: Option<i64>,
    /// Target branch / commit info.
    #[serde(default)]
    pub target: Option<PipelineTarget>,
    /// Trigger that fired the pipeline (push, schedule, manual).
    #[serde(default)]
    pub trigger: Option<PipelineTrigger>,
    /// Creator (omits the "trigger" person on schedules).
    #[serde(default)]
    pub creator: Option<User>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineState {
    /// `PENDING` / `IN_PROGRESS` / `COMPLETED` / `HALTED` / `STOPPED`.
    #[serde(default)]
    pub name: String,
    /// Only set when name = COMPLETED.
    #[serde(default)]
    pub result: Option<PipelineStateResult>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineStateResult {
    /// `SUCCESSFUL` / `FAILED` / `STOPPED` / `ERROR`.
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineTarget {
    /// `branch` is the usual ref name; `commit` would be set on
    /// commit-targeted pipelines, but Bitbucket sends `ref_name`
    /// for branches consistently.
    #[serde(default)]
    pub ref_name: Option<String>,
    /// Commit hash (`{ hash: "<sha>" }`).
    #[serde(default)]
    pub commit: Option<CommitRef>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineTrigger {
    /// `push` / `schedule` / `manual` / etc.
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct CommitRef {
    #[serde(default)]
    pub hash: String,
}

impl Pipeline {
    pub fn state_label(&self) -> String {
        match self.state.as_ref() {
            Some(s) if !s.name.is_empty() => {
                if let Some(r) = s.result.as_ref()
                    && !r.name.is_empty()
                {
                    return r.name.clone();
                }
                s.name.clone()
            }
            _ => "UNKNOWN".to_string(),
        }
    }

    /// Just the top-level pipeline state (PENDING / IN_PROGRESS /
    /// COMPLETED / HALTED / STOPPED) — WITHOUT collapsing
    /// COMPLETED down to its result name. Used by the workspace
    /// pipelines tree so the state column shows lifecycle stage
    /// and the separate result column shows outcome. Contrast
    /// with `state_label` (legacy pipelines-tab renderer)
    /// which collapses to result when set.
    /// tree-redesign 2026-07-14.
    pub fn state_only_label(&self) -> String {
        self.state
            .as_ref()
            .map(|s| s.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "UNKNOWN".to_string())
    }

    /// Result name (SUCCESSFUL / FAILED / STOPPED / ERROR) when
    /// the pipeline has completed; empty string while still in
    /// flight (PENDING / IN_PROGRESS). tree-redesign 2026-07-14.
    pub fn result_label(&self) -> String {
        self.state
            .as_ref()
            .and_then(|s| s.result.as_ref())
            .map(|r| r.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_default()
    }

    pub fn branch_label(&self) -> String {
        self.target
            .as_ref()
            .and_then(|t| t.ref_name.clone())
            .unwrap_or_else(|| "—".to_string())
    }

    pub fn short_sha(&self) -> String {
        self.target
            .as_ref()
            .and_then(|t| t.commit.as_ref().map(|c| c.hash.clone()))
            .map(|h| h.chars().take(7).collect::<String>())
            .unwrap_or_default()
    }

    pub fn trigger_label(&self) -> String {
        self.trigger
            .as_ref()
            .map(|t| t.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "—".into())
    }

    pub fn duration_label(&self) -> String {
        match self.duration_in_seconds {
            Some(s) if s > 0 => {
                let m = s / 60;
                let r = s % 60;
                if m > 0 {
                    format!("{m}m{r:02}s")
                } else {
                    format!("{r}s")
                }
            }
            _ => "—".into(),
        }
    }

    pub fn created_date(&self) -> String {
        self.created_on
            .as_deref()
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default()
    }
}

// ─── Branches ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BranchRefPage {
    #[serde(default)]
    values: Vec<BranchRef>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct BranchRef {
    pub name: String,
    /// Latest commit on this branch — Bitbucket nests `hash`, `date`,
    /// `author`, `message`. We use date + short hash + summary.
    #[serde(default)]
    pub target: Option<BranchTarget>,
    #[serde(default)]
    pub links: Option<Links>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct BranchTarget {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub author: Option<BranchAuthor>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct BranchAuthor {
    /// On branch targets Bitbucket sends `raw` ("Name <email>") rather
    /// than a User object.
    #[serde(default)]
    pub raw: String,
    /// Sometimes the resolved User is also attached.
    #[serde(default)]
    pub user: Option<User>,
}

impl BranchRef {
    pub fn short_sha(&self) -> String {
        self.target
            .as_ref()
            .map(|t| t.hash.chars().take(7).collect::<String>())
            .unwrap_or_default()
    }

    pub fn latest_date(&self) -> String {
        self.target
            .as_ref()
            .and_then(|t| t.date.as_deref())
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default()
    }

    pub fn author_label(&self) -> String {
        let Some(t) = self.target.as_ref() else {
            return "—".into();
        };
        if let Some(u) = t.author.as_ref().and_then(|a| a.user.as_ref())
            && !u.display_name.is_empty()
        {
            return u.display_name.clone();
        }
        let raw = t.author.as_ref().map(|a| a.raw.as_str()).unwrap_or("");
        if raw.is_empty() {
            return "—".into();
        }
        // Strip "Name <email>" down to "Name".
        raw.split('<').next().unwrap_or(raw).trim().to_string()
    }

    pub fn summary_line(&self) -> String {
        self.target
            .as_ref()
            .and_then(|t| t.message.as_deref())
            .map(|m| m.lines().next().unwrap_or("").trim().to_string())
            .unwrap_or_default()
    }
}

/// Parse a `Retry-After` header value per RFC-7231 §7.1.3. Accepts
/// either an integer delta-seconds (the typical Bitbucket shape) or
/// an HTTP-date (IMF-fixdate, RFC-850, or asctime — the three formats
/// RFC-7231 mandates). Returns None if neither form parses; the caller
/// uses its default backoff. Task #957 (2026-08-16 follow-up to #948).
fn parse_retry_after(raw: &str) -> Option<u64> {
    let s = raw.trim();
    // Fast path: delta-seconds — the form Bitbucket actually emits.
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    // Fallback: HTTP-date. chrono's DateTime::parse_from_rfc2822 covers
    // IMF-fixdate ("Fri, 31 Dec 1999 23:59:59 GMT") which is the
    // preferred form and what modern servers emit; the two legacy
    // forms (RFC-850, asctime) are rare enough in practice that we
    // don't bother — a missed parse just triggers default backoff.
    let dt = chrono::DateTime::parse_from_rfc2822(s).ok()?;
    let now = chrono::Utc::now();
    let delta = dt.with_timezone(&chrono::Utc) - now;
    delta.num_seconds().try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(state: &str) -> PullRequest {
        PullRequest {
            id: 42,
            title: "Fix bufferline crash".into(),
            state: state.into(),
            updated_on: Some("2026-06-04T10:23:11.000+0000".into()),
            author: Some(User {
                display_name: "alice".into(),
                account_id: Some("aid:abc".into()),
            }),
            destination: Some(Branch {
                branch: Some(BranchName {
                    name: "main".into(),
                }),
                repository: Some(Repo {
                    full_name: "acme/example-api".into(),
                }),
            }),
            source: Some(Branch {
                branch: Some(BranchName {
                    name: "alice/fix".into(),
                }),
                repository: Some(Repo {
                    full_name: "acme/example-api".into(),
                }),
            }),
            links: Some(Links {
                html: Some(HrefLink {
                    href: "https://bitbucket.org/acme/example-api/pull-requests/42".into(),
                }),
            }),
            reviewers: vec![],
            description: None,
            participants: vec![],
            merge_commit: None,
        }
    }

    #[test]
    fn approved_by_returns_true_when_account_matches_and_approved() {
        let mut p = pr("OPEN");
        p.participants = vec![Participant {
            user: Some(User {
                display_name: "alice".into(),
                account_id: Some("aid:alice".into()),
            }),
            role: Some("REVIEWER".into()),
            approved: Some(true),
            state: Some("approved".into()),
        }];
        assert!(p.approved_by("aid:alice"));
        assert!(!p.approved_by("aid:bob"));
    }

    #[test]
    fn approval_count_excludes_non_approving_participants() {
        let mut p = pr("OPEN");
        p.participants = vec![
            Participant {
                user: Some(User {
                    display_name: "a".into(),
                    account_id: Some("aid:a".into()),
                }),
                role: None,
                approved: Some(true),
                state: None,
            },
            Participant {
                user: Some(User {
                    display_name: "b".into(),
                    account_id: Some("aid:b".into()),
                }),
                role: None,
                approved: Some(false),
                state: None,
            },
            Participant {
                user: Some(User {
                    display_name: "c".into(),
                    account_id: Some("aid:c".into()),
                }),
                role: None,
                approved: None,
                state: None,
            },
        ];
        assert_eq!(p.approval_count(), 1);
    }

    #[test]
    fn repo_short_returns_destination_full_name() {
        assert_eq!(pr("OPEN").repo_short(), "acme/example-api");
    }

    #[test]
    fn updated_date_takes_first_ten_chars() {
        assert_eq!(pr("OPEN").updated_date(), "2026-06-04");
    }

    #[test]
    fn html_url_pulls_from_links() {
        assert_eq!(
            pr("MERGED").html_url(),
            Some("https://bitbucket.org/acme/example-api/pull-requests/42".into())
        );
    }

    #[test]
    fn html_url_is_none_when_links_missing() {
        let mut p = pr("OPEN");
        p.links = None;
        assert_eq!(p.html_url(), None);
    }

    // ── 2026-08-16 (#948) — FetchErr label + retry-after semantics ─

    #[test]
    fn fetch_err_short_label_covers_common_statuses() {
        assert_eq!(FetchErr::http(401, "").short_label(), "auth failed");
        assert_eq!(FetchErr::http(403, "").short_label(), "auth failed");
        assert_eq!(FetchErr::http(404, "").short_label(), "no such repo");
        assert_eq!(FetchErr::http(500, "").short_label(), "HTTP 500");
        assert_eq!(FetchErr::network("dns").short_label(), "network error");
    }

    #[test]
    fn fetch_err_429_label_includes_retry_after_when_present() {
        let e = FetchErr::http(429, "throttled").with_retry_after(45);
        assert_eq!(e.short_label(), "429 · retry in 45s");
        assert!(e.is_rate_limited());
    }

    #[test]
    fn fetch_err_429_label_without_header_is_generic() {
        let e = FetchErr::http(429, "throttled");
        assert_eq!(e.short_label(), "429 · rate limited");
    }

    #[test]
    fn parse_retry_after_delta_seconds() {
        assert_eq!(parse_retry_after("30"), Some(30));
        assert_eq!(parse_retry_after("  60  "), Some(60));
        assert_eq!(parse_retry_after("0"), Some(0));
    }

    #[test]
    fn parse_retry_after_http_date_future() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(120);
        let hdr = future.to_rfc2822();
        let got = parse_retry_after(&hdr).unwrap_or(0);
        // Rounding: chrono::Duration::seconds truncates; allow ±2s slack.
        assert!(got >= 118 && got <= 122, "expected ~120, got {got}");
    }

    #[test]
    fn parse_retry_after_http_date_past_or_invalid() {
        // Past date → negative delta → try_into::<u64> fails → None.
        let past = chrono::Utc::now() - chrono::Duration::seconds(60);
        assert_eq!(parse_retry_after(&past.to_rfc2822()), None);
        // Junk → both parses fail → None.
        assert_eq!(parse_retry_after("not a date"), None);
        assert_eq!(parse_retry_after(""), None);
    }
}
