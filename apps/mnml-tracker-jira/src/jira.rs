//! Minimal Jira REST API client — only the endpoints we need.
//!
//! Uses HTTP Basic auth with `email:api_token`. The `Client` is
//! `Clone` and cheap to copy across tasks (it holds an `Arc`-backed
//! `reqwest::Client` internally).

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    email: String,
    token: String,
}

impl Client {
    pub fn new(base_url: &str, email: &str, token: &str) -> Result<Self> {
        let base = base_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .user_agent("mnml-tracker-jira/0.1.0")
            .build()?;
        Ok(Self {
            http,
            base,
            email: email.to_string(),
            token: token.to_string(),
        })
    }

    /// Run a JQL query. Returns up to `max_results` issues (the
    /// Jira API caps this at 100).
    ///
    /// 2026-07-25 — migrated from `/rest/api/3/search` to
    /// `/rest/api/3/search/jql`. Atlassian retired the old
    /// endpoint (returns 410 Gone with a migration pointer).
    /// The new endpoint's request shape is a superset — same
    /// `jql` / `fields` / `maxResults`, plus optional
    /// `nextPageToken` for pagination. Response shape drops the
    /// `total` field (was deprecated anyway) and adds
    /// `isLast` / `nextPageToken`; we only read `issues` so
    /// SearchResult is unchanged.
    /// `extra_fields` — additional Jira field ids to include in the
    /// response (e.g. a custom-field id like `"customfield_10056"`
    /// for a team-selector). Emitted alongside the fixed default set.
    pub async fn search(
        &self,
        jql: &str,
        max_results: u32,
        extra_fields: &[String],
    ) -> Result<Vec<Issue>> {
        let url = format!("{}/rest/api/3/search/jql", self.base);
        let mut fields: Vec<String> = [
            "summary",
            "status",
            "assignee",
            "reporter",
            "priority",
            "issuetype",
            "updated",
            "created",
            "fixVersions",
            "components",
            "labels",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Caller-supplied field ids (e.g. a team custom-field from the
        // tracker config). Lands in `Fields.extras` via
        // `#[serde(flatten)]`.
        fields.extend(extra_fields.iter().cloned());
        // Paginate — the new /search/jql endpoint returns `isLast`
        // + `nextPageToken` when there are more pages. Prior code
        // silently truncated at `max_results`. Task #1016.
        let mut all: Vec<Issue> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut body = serde_json::json!({
                "jql": jql,
                "maxResults": max_results,
                "fields": fields,
            });
            if let Some(tok) = next_token.as_ref() {
                body["nextPageToken"] = serde_json::Value::String(tok.clone());
            }
            let resp = self
                .http
                .post(&url)
                .basic_auth(&self.email, Some(&self.token))
                .header("Accept", "application/json")
                .json(&body)
                .send()
                .await
                .context("Jira search request failed")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("Jira search failed: {status}: {text}"));
            }
            let sr: SearchResult = resp.json().await.context("parsing Jira search response")?;
            all.extend(sr.issues);
            if all.len() >= MAX_PAGINATION_ISSUES {
                all.truncate(MAX_PAGINATION_ISSUES);
                break;
            }
            match sr.next_page_token {
                Some(tok) if !sr.is_last.unwrap_or(false) => {
                    next_token = Some(tok);
                }
                _ => break,
            }
        }
        Ok(all)
    }

    /// Fetch the unreleased versions of `project`, ordered by start
    /// date ascending (so `[0]` is the next-up release).
    pub async fn unreleased_versions(&self, project_key: &str) -> Result<Vec<ProjectVersion>> {
        let url = format!("{}/rest/api/3/project/{project_key}/versions", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching versions for project {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira versions fetch failed: {status}: {text}"));
        }
        let mut versions: Vec<ProjectVersion> = resp
            .json()
            .await
            .context("parsing Jira versions response")?;
        versions.retain(|v| !v.released);
        // 2026-07-26 — sort by startDate ascending (None last),
        // then name DESCENDING as a fallback. Rationale: most
        // projects don't set startDate on release-version records,
        // so the fallback is what actually runs. Semver-style names
        // sorted descending put the highest-numbered version first
        // (13.16.0 > 13.15.0 > 13.14.1) — matches the user's
        // intuition that "Current Release" = the newest one being
        // worked on, not the oldest still-unreleased placeholder.
        versions.sort_by(
            |a, b| match (a.start_date.as_deref(), b.start_date.as_deref()) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.name.cmp(&a.name), // descending: newest first
            },
        );
        Ok(versions)
    }

    /// Browser URL for a given issue key (e.g. `TE-1234`).
    pub fn issue_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.base)
    }

    /// The trailing-slash-stripped base URL (`https://foo.atlassian.net`).
    /// Callers that build a Jira URL outside this client's endpoints
    /// (e.g. the board settings page, task #893) borrow this instead
    /// of duplicating the trim.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// 2026-07-25 — list PRs linked to a Jira issue via the Atlassian
    /// dev-status API. Requires the Bitbucket-for-Jira (or GitHub /
    /// Azure DevOps / Stash) app connector to be installed in the
    /// Jira workspace — that's what populates the dev-status data
    /// backing the "Development" panel in Jira's own issue UI.
    ///
    /// Returns an empty vec (not an error) when no PRs are linked,
    /// when the connector isn't installed, or when the applicationType
    /// isn't supported. Downstream callers render "no linked PRs".
    ///
    /// `application_type` is the connector kind — `"bitbucket"`,
    /// `"github"`, `"stash"`, `"azure"`. Bitbucket is a common choice.
    pub async fn list_prs_for_issue(
        &self,
        issue_id: &str,
        application_type: &str,
    ) -> Result<Vec<LinkedPr>> {
        let url = format!("{}/rest/dev-status/latest/issue/detail", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .query(&[
                ("issueId", issue_id),
                ("applicationType", application_type),
                ("dataType", "pullrequest"),
            ])
            .send()
            .await
            .with_context(|| format!("dev-status PR fetch for issue {issue_id}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // 404 = no dev-info; treat as "no PRs" so we don't spam
            // errors for freshly-created tickets. Other statuses
            // still surface (auth, rate limit).
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(Vec::new());
            }
            return Err(anyhow!("dev-status PR fetch: {status}: {text}"));
        }
        let sr: DevStatusResponse = resp
            .json()
            .await
            .context("parsing dev-status PR response")?;
        // The `detail` array has one entry per repository the issue
        // touches. Flatten across all repos — the UI groups by repo
        // slug (populated from `repositoryName`) later.
        Ok(sr
            .detail
            .into_iter()
            .flat_map(|d| d.pull_requests)
            .collect())
    }

    /// Fetch the workflow transitions available for `key`. Different
    /// per-issue depending on the project's workflow + the current
    /// status (Jira's workflow engine is graph-based; you can only
    /// see outgoing edges from the current node). Empty list is
    /// valid — it just means the user has no transitions available
    /// (lacks permission, or a terminal state with no outgoing edges).
    pub async fn fetch_transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = format!("{}/rest/api/3/issue/{key}/transitions", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching transitions for {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira transitions fetch failed for {key}: {status}: {text}"
            ));
        }
        let raw: TransitionsRaw = resp
            .json()
            .await
            .with_context(|| format!("parsing transitions for {key}"))?;
        Ok(raw
            .transitions
            .into_iter()
            .map(|t| Transition {
                id: t.id,
                name: t.name,
                to_name: t.to.as_ref().map(|s| s.name.clone()),
            })
            .collect())
    }

    /// Add the authenticated user as a watcher of `key`. Jira's POST
    /// endpoint with an empty body watches as the basic-auth user, so
    /// we don't need the accountId for this direction.
    pub async fn watch_issue(&self, key: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/watchers", self.base);
        // The endpoint accepts an empty string for "current user".
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .body("\"\"")
            .send()
            .await
            .with_context(|| format!("watching {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira watch failed for {key}: {status}: {text}"));
        }
        Ok(())
    }

    /// Drop `account_id` from the watcher list of `key`. The accountId
    /// is required by the DELETE endpoint; for the authenticated-user
    /// case fetch it once via [`Self::myself`] and pass it in.
    pub async fn unwatch_issue(&self, key: &str, account_id: &str) -> Result<()> {
        let url = format!(
            "{}/rest/api/3/issue/{key}/watchers?accountId={account_id}",
            self.base
        );
        let resp = self
            .http
            .delete(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("unwatching {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira unwatch failed for {key}: {status}: {text}"));
        }
        Ok(())
    }

    /// Return the authenticated user's accountId. Required for the
    /// unwatch DELETE call; cache it once per session at the call
    /// site (App owns the cache, not the Client — keeps the Client
    /// stateless / re-runnable).
    pub async fn myself(&self) -> Result<String> {
        let url = format!("{}/rest/api/3/myself", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .context("fetching authenticated user")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira myself failed: {status}: {text}"));
        }
        let raw: MyselfRaw = resp.json().await.context("parsing myself response")?;
        Ok(raw.account_id)
    }

    /// Fetch users assignable to `project_key`, narrowed by `query`
    /// (Jira does the substring match server-side). Used by the `a`
    /// assignee picker — pre-fetched on first open per-project, then
    /// re-queried as the user types if more than a small page is
    /// available.
    pub async fn fetch_assignable_users(
        &self,
        project_key: &str,
        query: &str,
    ) -> Result<Vec<User>> {
        // The `query` param is the case-insensitive substring filter;
        // empty `query` returns the first page of all assignable users.
        let url = format!(
            "{}/rest/api/3/user/assignable/search?project={project_key}&query={query}&maxResults=50",
            self.base
        );
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching assignable users for {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira assignable users fetch failed for {project_key}: {status}: {text}"
            ));
        }
        let users: Vec<UserWithId> = resp
            .json()
            .await
            .with_context(|| format!("parsing assignable users for {project_key}"))?;
        Ok(users
            .into_iter()
            .map(|u| User {
                display_name: u.display_name,
                account_id: u.account_id,
            })
            .collect())
    }

    /// 2026-08-07 — issues in the board's active sprint (or all
    /// board issues when no sprint is active). Mirrors what Jira's
    /// own UI shows for `.../boards/{id}`. Extra jql AND-clauses are
    /// appended via the `?jql=` query param (Jira Agile API supports
    /// this). `extra_fields` matches `search()`'s param.
    pub async fn fetch_board_issues(
        &self,
        board_id: u64,
        extra_jql: Option<&str>,
        extra_fields: &[String],
    ) -> Result<Vec<Issue>> {
        let mut fields: Vec<String> = [
            "summary",
            "status",
            "assignee",
            "reporter",
            "priority",
            "issuetype",
            "updated",
            "created",
            "fixVersions",
            "components",
            "labels",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        fields.extend(extra_fields.iter().cloned());
        let fields_csv = fields.join(",");
        let url = format!("{}/rest/agile/1.0/board/{board_id}/issue", self.base);
        // Paginate — Agile REST caps at maxResults=100 per page and
        // returns startAt/total for the caller to loop on. Prior
        // single-page code silently truncated every board bigger than
        // 100 (user report 2026-08-18: "Sprint · 100 issues" on
        // HeliOS was actually 100+, not exactly 100). Task #1016.
        let mut all: Vec<Issue> = Vec::new();
        let mut start_at: u32 = 0;
        let page_size: u32 = 100;
        loop {
            let start_str = start_at.to_string();
            let max_str = page_size.to_string();
            let mut req = self
                .http
                .get(&url)
                .basic_auth(&self.email, Some(&self.token))
                .header("Accept", "application/json")
                .query(&[
                    ("fields", fields_csv.as_str()),
                    ("maxResults", max_str.as_str()),
                    ("startAt", start_str.as_str()),
                ]);
            if let Some(j) = extra_jql
                && !j.trim().is_empty()
            {
                req = req.query(&[("jql", j)]);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("board {board_id} issues fetch"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("board {board_id} issues fetch: {status}: {text}"));
            }
            let sr: SearchResult = resp.json().await.context("parsing board issues response")?;
            let got = sr.issues.len() as u32;
            all.extend(sr.issues);
            if all.len() >= MAX_PAGINATION_ISSUES {
                // Sanity ceiling. Truncate + stop. Caller sees a
                // capped list; a follow-up toast at the app layer
                // could surface the truncation but that's optional.
                all.truncate(MAX_PAGINATION_ISSUES);
                break;
            }
            // Terminate when the page returned less than requested
            // (last page) or when startAt + got >= total (Agile
            // returns `total` reliably; new /search/jql doesn't, so
            // check `is_last` too).
            let done_by_size = got < page_size;
            let done_by_total = sr.total.map(|t| (start_at + got) >= t).unwrap_or(false);
            let done_by_flag = sr.is_last.unwrap_or(false);
            if done_by_size || done_by_total || done_by_flag {
                break;
            }
            start_at += page_size;
        }
        Ok(all)
    }

    /// 2026-08-07 — fetch a single board's metadata by id. Used by
    /// the toolbar to render a friendly `[Board: HeliOS]` chip
    /// instead of the numeric `[Board:200]`.
    pub async fn fetch_board(&self, board_id: u64) -> Result<Board> {
        let url = format!("{}/rest/agile/1.0/board/{board_id}", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching board {board_id}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("board {board_id} fetch: {status}: {text}"));
        }
        let board: Board = resp
            .json()
            .await
            .with_context(|| format!("parsing board {board_id} metadata"))?;
        Ok(board)
    }

    /// 2026-08-07 — fetch one issue with the caller-specified field
    /// list (any mix of canonical + `customfield_XXXXX`). Used by
    /// the detail modal to pull whatever the user's `[detail_modal]
    /// fields = [...]` config asks for. Returns a raw JSON blob so
    /// the caller can render fields it doesn't have serde structs
    /// for (custom fields, environment, sprint sub-structs).
    pub async fn fetch_issue_full(
        &self,
        key: &str,
        fields: &[String],
    ) -> Result<serde_json::Value> {
        let fields_csv = if fields.is_empty() {
            "*all".to_string()
        } else {
            fields.join(",")
        };
        let url = format!("{}/rest/api/3/issue/{key}", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .query(&[("fields", fields_csv.as_str())])
            .send()
            .await
            .with_context(|| format!("fetching issue {key} full"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("issue {key} full fetch: {status}: {text}"));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("parsing issue {key} full"))?;
        Ok(v)
    }

    /// 2026-08-07 — list boards visible to the current user for a
    /// project. Used by the board-selector chip. Board.type is
    /// "scrum" | "kanban" | "simple".
    pub async fn fetch_boards_for_project(&self, project_key: &str) -> Result<Vec<Board>> {
        let url = format!("{}/rest/agile/1.0/board", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .query(&[("projectKeyOrId", project_key), ("maxResults", "100")])
            .send()
            .await
            .with_context(|| format!("fetching boards for project {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("board list fetch: {status}: {text}"));
        }
        let br: BoardListResponse = resp.json().await.context("parsing board list response")?;
        Ok(br.values)
    }

    /// 2026-08-17 (task #887) — list the sprints for a board. The
    /// Agile API's `state` query param takes any comma-separated
    /// subset of `active,future,closed`; we always ask for all three
    /// so the caller can render current + upcoming + last-N-closed
    /// in one round trip.
    ///
    /// Returns an empty vec (not an error) for kanban boards where
    /// the endpoint replies 400 "The board does not support sprints"
    /// — the UI hides the sprint chip in that case rather than
    /// surfacing a toast on every refresh.
    ///
    /// Pagination: the endpoint is paged (`isLast` / `startAt` /
    /// `maxResults`); we ask for `maxResults=50` in one shot which
    /// covers the practical case of "current + a handful of future
    /// + the last ~10 closed". If a board has more, older closed
    ///   sprints simply don't surface — the caller trims to
    ///   `last N closed` anyway.
    pub async fn fetch_sprints_for_board(&self, board_id: u64) -> Result<Vec<Sprint>> {
        // 2026-08-18 (task #887 follow-up) — Jira's `/board/{id}/sprint`
        // paginates in CREATION ORDER (oldest first). On boards with
        // 700+ closed sprints, the first-page combined query returned
        // only ancient closed sprints — the current active sprint
        // (highest id) never appeared. Fix: request each state
        // SEPARATELY. Active + future are cheap (usually <10 total).
        // Closed uses startAt=(total - N) to grab the MOST RECENT
        // closed sprints; the picker caps at 5 anyway, take 20 for
        // sort headroom.
        let mut out: Vec<Sprint> = Vec::new();
        for state in ["active", "future"] {
            match self.fetch_sprints_state(board_id, state, 0, 50).await {
                Ok(mut list) => out.append(&mut list),
                // 400 = kanban board (no sprints on ANY state). Same
                // handling as the prior single-request path.
                Err(e) if e.to_string().contains(" 400") => return Ok(Vec::new()),
                Err(e) => return Err(e),
            }
        }
        // Closed: get total, jump to (total - headroom) for the recent tail.
        let closed_headroom = 20u32;
        match self.fetch_sprints_state_total(board_id, "closed").await {
            Ok(total) => {
                let start = total.saturating_sub(closed_headroom);
                if let Ok(mut recent) = self
                    .fetch_sprints_state(board_id, "closed", start, closed_headroom)
                    .await
                {
                    out.append(&mut recent);
                }
            }
            Err(_) => { /* no closed sprints or transient; active/future already in out */ }
        }
        Ok(out)
    }

    async fn fetch_sprints_state(
        &self,
        board_id: u64,
        state: &str,
        start_at: u32,
        max_results: u32,
    ) -> Result<Vec<Sprint>> {
        let url = format!("{}/rest/agile/1.0/board/{board_id}/sprint", self.base);
        let start_str = start_at.to_string();
        let max_str = max_results.to_string();
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .query(&[
                ("state", state),
                ("startAt", start_str.as_str()),
                ("maxResults", max_str.as_str()),
            ])
            .send()
            .await
            .with_context(|| format!("fetching {state} sprints for board {board_id}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "sprint list fetch failed for board {board_id} state={state}: {status}: {text}"
            ));
        }
        let sr: SprintListResponse = resp
            .json()
            .await
            .with_context(|| format!("parsing sprint list for board {board_id}"))?;
        Ok(sr.values)
    }

    async fn fetch_sprints_state_total(&self, board_id: u64, state: &str) -> Result<u32> {
        #[derive(serde::Deserialize)]
        struct TotalOnly {
            #[serde(default)]
            total: u32,
        }
        let url = format!("{}/rest/agile/1.0/board/{board_id}/sprint", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .query(&[("state", state), ("startAt", "0"), ("maxResults", "1")])
            .send()
            .await?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("sprint total fetch failed: {text}"));
        }
        let t: TotalOnly = resp.json().await?;
        Ok(t.total)
    }

    /// 2026-08-17 (task #893) — list the board's saved "quick
    /// filters". These are user-defined JQL fragments (typically
    /// `assignee = currentUser()`, `labels = "hotfix"`) that Jira
    /// Cloud's board toolbar surfaces as toggleable chips. Each has
    /// a stable id + a display name + a JQL fragment we can layer
    /// into the board fetch via `?jql=<extra>`.
    ///
    /// Returns an empty vec (not an error) if the board defines no
    /// quick filters. The UI collapses the chip in that case.
    pub async fn fetch_quickfilters_for_board(&self, board_id: u64) -> Result<Vec<QuickFilter>> {
        let url = format!("{}/rest/agile/1.0/board/{board_id}/quickfilter", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .query(&[("maxResults", "50")])
            .send()
            .await
            .with_context(|| format!("fetching quick filters for board {board_id}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "quick filter fetch failed for board {board_id}: {status}: {text}"
            ));
        }
        let sr: QuickFilterListResponse = resp
            .json()
            .await
            .with_context(|| format!("parsing quick filters for board {board_id}"))?;
        Ok(sr.values)
    }

    /// Fetch every version of `project_key` (released + unreleased,
    /// archived skipped). Sorted by startDate desc then name — the
    /// most-recent / next-up versions show up first.
    pub async fn fetch_versions(&self, project_key: &str) -> Result<Vec<ProjectVersion>> {
        let url = format!("{}/rest/api/3/project/{project_key}/versions", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching versions for {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira versions fetch failed for {project_key}: {status}: {text}"
            ));
        }
        let mut versions: Vec<ProjectVersion> = resp
            .json()
            .await
            .with_context(|| format!("parsing versions for {project_key}"))?;
        versions.retain(|v| !v.archived);
        // 2026-08-06 — unreleased FIRST so release-planning versions
        // (13.16.0 / 13.17.0 etc) never get pushed off the picker's
        // visible list by older released versions. Was: sorted purely
        // by startDate descending — versions with no startDate sank
        // to the bottom, which is exactly where 13.16.0 / 13.17.0
        // live (product creates them without a date until the
        // release cut). User: "why isn't 13.16.0 in the picker."
        //
        // Within each bucket (unreleased / released), sort by
        // startDate desc; missing dates fall back to name desc so
        // 13.17.0 > 13.16.0 alphabetically when neither has a date.
        versions.sort_by(|a, b| {
            let a_unreleased = !a.released;
            let b_unreleased = !b.released;
            // Unreleased before released.
            if a_unreleased != b_unreleased {
                return b_unreleased.cmp(&a_unreleased);
            }
            match (a.start_date.as_deref(), b.start_date.as_deref()) {
                (Some(x), Some(y)) => y.cmp(x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.name.cmp(&a.name),
            }
        });
        Ok(versions)
    }

    /// PUT a new assignee on `key`. Empty `account_id` ⇒ unassign.
    pub async fn set_assignee(&self, key: &str, account_id: Option<&str>) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base);
        let assignee = match account_id {
            Some(id) => serde_json::json!({ "accountId": id }),
            None => serde_json::Value::Null,
        };
        let body = serde_json::json!({ "fields": { "assignee": assignee } });
        let resp = self
            .http
            .put(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("setting assignee on {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira assignee set failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// PUT a fixVersion list on `key`. Empty Vec ⇒ clear fixVersions.
    pub async fn set_fix_versions(&self, key: &str, version_names: &[String]) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base);
        let versions: Vec<serde_json::Value> = version_names
            .iter()
            .map(|n| serde_json::json!({ "name": n }))
            .collect();
        let body = serde_json::json!({ "fields": { "fixVersions": versions } });
        let resp = self
            .http
            .put(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("setting fixVersions on {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira fixVersions set failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// POST a plain-text comment to `key`. The body gets wrapped in
    /// the minimal ADF JSON the v3 API requires — one paragraph per
    /// line in `text`, blank lines become empty paragraphs.
    pub async fn post_comment(&self, key: &str, text: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/comment", self.base);
        let body = serde_json::json!({
            "body": plain_to_adf(text),
        });
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("posting comment on {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira comment post failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// Fire a workflow transition by id. Returns `Ok(())` on success
    /// (Jira returns 204 No Content on a successful transition).
    pub async fn run_transition(&self, key: &str, transition_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/transitions", self.base);
        let body = serde_json::json!({
            "transition": { "id": transition_id }
        });
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("posting transition for {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira transition failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// Fetch a single issue's description + comments + watch state.
    /// The fields already on `Issue` (status, assignee, …) are
    /// included too so the detail view can re-read updated state
    /// without a stale pre-detail fetch.
    pub async fn fetch_issue_detail(&self, key: &str) -> Result<IssueDetail> {
        let url = format!(
            "{}/rest/api/3/issue/{key}?fields=description,comment,watches,summary,status,assignee,issuetype,priority,fixVersions,updated,reporter",
            self.base
        );
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching issue {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira issue fetch failed for {key}: {status}: {text}"
            ));
        }
        let raw: IssueDetailRaw = resp
            .json()
            .await
            .with_context(|| format!("parsing detail for {key}"))?;
        let description = raw
            .fields
            .description
            .as_ref()
            .map(adf_to_text)
            .filter(|s| !s.trim().is_empty());
        let comments = raw
            .fields
            .comment
            .map(|c| {
                c.comments
                    .into_iter()
                    .map(|raw| Comment {
                        author: raw.author.as_ref().map(|u| u.display_name.clone()),
                        created: raw.created,
                        body: raw.body.as_ref().map(adf_to_text).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let watches = raw.fields.watches.unwrap_or_default();
        Ok(IssueDetail {
            description,
            comments,
            watching: watches.is_watching,
            watch_count: watches.watch_count,
        })
    }
}

/// One ticket's narrative content — description + the comment thread,
/// plus watch state. Lazy-loaded per-issue when the detail pane opens
/// or `w` is pressed.
#[derive(Debug, Clone, Default)]
pub struct IssueDetail {
    pub description: Option<String>,
    pub comments: Vec<Comment>,
    /// True when the authenticated user is currently a watcher of the
    /// issue. Drives the watcher chip + the `w` toggle direction.
    pub watching: bool,
    /// Total watcher count (including non-self). Surfaces in the
    /// detail header so the user can see whether anyone else cares.
    pub watch_count: u32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MyselfRaw {
    #[serde(rename = "accountId")]
    account_id: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub author: Option<String>,
    pub created: Option<String>,
    pub body: String,
}

/// One outgoing workflow edge from the issue's current status. The
/// `to_name` is the resulting status (e.g. "In Review"); `name` is
/// the *button label* the user clicks in Jira's UI (e.g. "Start review")
/// which can differ from the destination state.
#[derive(Debug, Clone)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TransitionsRaw {
    transitions: Vec<TransitionRaw>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TransitionRaw {
    id: String,
    name: String,
    #[serde(default)]
    to: Option<NamedField>,
}

#[derive(Debug, Deserialize)]
struct IssueDetailRaw {
    fields: IssueDetailFieldsRaw,
}

#[derive(Debug, Deserialize)]
struct IssueDetailFieldsRaw {
    #[serde(default)]
    description: Option<serde_json::Value>,
    #[serde(default)]
    comment: Option<CommentListRaw>,
    #[serde(default)]
    watches: Option<WatchesRaw>,
}

#[derive(Debug, Default, Deserialize)]
struct WatchesRaw {
    #[serde(default, rename = "watchCount")]
    watch_count: u32,
    #[serde(default, rename = "isWatching")]
    is_watching: bool,
}

#[derive(Debug, Deserialize)]
struct CommentListRaw {
    #[serde(default)]
    comments: Vec<CommentRaw>,
}

#[derive(Debug, Deserialize)]
struct CommentRaw {
    #[serde(default)]
    author: Option<User>,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    body: Option<serde_json::Value>,
}

/// Plain text → minimal ADF JSON. Inverse of [`adf_to_text`]; used
/// when posting comments back to Jira. One paragraph per non-empty
/// input line; blank lines pass through as empty paragraphs so the
/// reader sees the same visual break.
pub(crate) fn plain_to_adf(text: &str) -> serde_json::Value {
    let paragraphs: Vec<serde_json::Value> = text
        .lines()
        .map(|line| {
            if line.is_empty() {
                serde_json::json!({ "type": "paragraph" })
            } else {
                serde_json::json!({
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": line }]
                })
            }
        })
        .collect();
    serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": paragraphs,
    })
}

/// Atlassian Document Format → plain text. ADF is a recursive JSON
/// tree with `type` + `content` arrays + leaf `text` nodes. We walk
/// the tree, concatenate `text` values, and emit newlines for the
/// block-level types we care about (`paragraph`, `heading`, `bullet`
/// items, `code_block`). Inline formatting marks are stripped — the
/// detail pane is plain-text only in v1.
pub(crate) fn adf_to_text(v: &serde_json::Value) -> String {
    let mut out = String::new();
    walk_adf(v, &mut out);
    out
}

fn walk_adf(node: &serde_json::Value, out: &mut String) {
    if let Some(s) = node.get("text").and_then(|v| v.as_str()) {
        out.push_str(s);
    }
    if let Some(children) = node.get("content").and_then(|v| v.as_array()) {
        for child in children {
            walk_adf(child, out);
        }
    }
    let kind = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if matches!(
        kind,
        "paragraph"
            | "heading"
            | "codeBlock"
            | "blockquote"
            | "rule"
            | "listItem"
            | "bulletList"
            | "orderedList"
            | "hardBreak"
    ) && !out.ends_with('\n')
    {
        out.push('\n');
    }
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    issues: Vec<Issue>,
    /// Agile API returns `startAt` + `maxResults` + `total`; the
    /// newer `/rest/api/3/search/jql` returns `isLast` +
    /// `nextPageToken`. We accept either shape (all optional) and
    /// let the caller loop on whichever fires.
    #[serde(default)]
    start_at: Option<u32>,
    #[serde(default, rename = "maxResults")]
    max_results: Option<u32>,
    #[serde(default)]
    total: Option<u32>,
    #[serde(default, rename = "isLast")]
    is_last: Option<bool>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Sanity ceiling on pagination — never fetch more than this many
/// issues from one endpoint. Prevents runaway loops on misconfigured
/// boards / broken filters that would otherwise iterate forever.
/// 500 covers every realistic sprint / release / assigned-work scope
/// (Bitbucket workspace-wide open PRs is a rare exception, but that
/// lives in a different sibling).
const MAX_PAGINATION_ISSUES: usize = 500;

#[derive(Debug, Deserialize, Clone)]
pub struct Issue {
    /// Numeric internal ID — REQUIRED by the dev-status API to look
    /// up linked PRs. Jira's search endpoint returns it alongside
    /// `key`; keep both because the key is what humans see and the
    /// id is what the dev-status endpoint takes.
    pub id: String,
    pub key: String,
    pub fields: Fields,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Fields {
    pub summary: String,
    #[serde(default)]
    pub status: Option<NamedField>,
    #[serde(default)]
    pub assignee: Option<User>,
    /// Parsed but not yet rendered. Will surface in the planned
    /// per-tab column override + ticket detail panel.
    #[serde(default)]
    pub reporter: Option<User>,
    #[serde(default)]
    pub priority: Option<NamedField>,
    #[serde(default)]
    pub issuetype: Option<NamedField>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    #[serde(rename = "fixVersions")]
    pub fix_versions: Vec<NamedField>,
    /// Component names (e.g. "web-team", "mobile"). Populated when
    /// the JQL response asks for `components`.
    #[serde(default)]
    pub components: Vec<NamedField>,
    /// Freeform label strings (e.g. "team:web"). Same as above —
    /// requires `labels` in the requested field list.
    #[serde(default)]
    pub labels: Vec<String>,
    /// 2026-08-07 — extra fields for the team filter, keyed by the
    /// custom-field id the caller requested (e.g. `customfield_10056`
    /// for a select custom field (e.g. "Team")). Value is the option's
    /// `value` string ("HeliOS", "Atlas", etc). Populated by the
    /// custom flatten below when the response includes those keys.
    #[serde(flatten, default)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NamedField {
    pub name: String,
}

// ── dev-status API (linked PRs) ─────────────────────────────────
// 2026-07-25 — models the Atlassian dev-status endpoint response.
// One `detail` per repository the issue touches; each holds a list
// of `pullRequests`. Fields marked `#[serde(default)]` because the
// API returns different subsets depending on the connector version.

#[derive(Debug, Deserialize, Clone)]
struct DevStatusResponse {
    #[serde(default)]
    detail: Vec<DevStatusDetail>,
}

#[derive(Debug, Deserialize, Clone)]
struct DevStatusDetail {
    #[serde(rename = "pullRequests", default)]
    pull_requests: Vec<LinkedPr>,
}

/// One PR linked to a Jira issue via the dev-status API. Shape
/// matches what Atlassian returns; not every connector populates
/// every field.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct LinkedPr {
    /// e.g. "#2023" — includes the leading `#`. Ready to render.
    pub id: String,
    /// PR title.
    #[serde(default)]
    pub name: String,
    /// One of "OPEN", "MERGED", "DECLINED" (case varies by
    /// connector; render uppercase). Empty = unknown.
    #[serde(default)]
    pub status: String,
    /// bitbucket.org / github.com URL for the PR.
    #[serde(default)]
    pub url: String,
    /// Source branch info. Includes URL for cross-navigation.
    #[serde(default)]
    pub source: LinkedPrBranch,
    /// Destination (target) branch.
    #[serde(default)]
    pub destination: LinkedPrBranch,
    /// Reviewers list — approval bump rule counts these.
    #[serde(default)]
    pub reviewers: Vec<LinkedPrReviewer>,
    /// Author display name.
    #[serde(default)]
    pub author: LinkedPrAuthor,
    /// ISO timestamp of last update. Used for the UPDATED column.
    #[serde(rename = "lastUpdate", default)]
    pub last_update: String,
    /// Repo slug (e.g. "merchant-dashboard"). Used to group PRs
    /// under a ticket by repo when the ticket touches multiple.
    #[serde(rename = "repositoryName", default)]
    pub repository_name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct LinkedPrBranch {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct LinkedPrReviewer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct LinkedPrAuthor {
    #[serde(default)]
    pub name: String,
}

impl LinkedPr {
    /// True when at least one reviewer has approved. Feeds the
    /// Fix Versions "approved → bump higher" sort rule.
    pub fn is_approved(&self) -> bool {
        self.reviewers.iter().any(|r| r.approved)
    }

    /// True for OPEN/DRAFT PRs (any state that isn't merged /
    /// declined / superseded). Feeds the "no open PRs → dev
    /// probably forgot to transition to Testing" bump rule.
    pub fn is_open(&self) -> bool {
        matches!(
            self.status.to_ascii_uppercase().as_str(),
            "OPEN" | "DRAFT" | "IN_REVIEW"
        )
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct User {
    #[serde(rename = "displayName", default)]
    pub display_name: String,
    /// Atlassian accountId — present on `/user/assignable/search` and
    /// `/myself` responses but not the abbreviated user objects that
    /// appear inside `Issue.fields.assignee` etc. Empty string ⇒ not
    /// known (e.g. a legacy email-only assignee that didn't migrate
    /// to GDPR-mode accountIds).
    #[serde(default, rename = "accountId")]
    pub account_id: String,
}

/// Same shape as User but with `accountId` deserialized as the
/// required field — the assignable-search endpoint always returns it.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct UserWithId {
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "accountId")]
    account_id: String,
}

/// 2026-08-07 — one Jira Software board (scrum / kanban / simple).
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Board {
    pub id: u64,
    pub name: String,
    #[serde(default, rename = "type")]
    pub board_type: String,
}

#[derive(Debug, Deserialize, Clone)]
struct BoardListResponse {
    #[serde(default)]
    values: Vec<Board>,
}

/// 2026-08-17 (task #887) — one sprint on a Jira Software board.
/// State is one of `"active" | "future" | "closed"`. Dates are ISO
/// strings; `complete_date` is present only on closed sprints.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Sprint {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default, rename = "startDate")]
    pub start_date: Option<String>,
    #[serde(default, rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default, rename = "completeDate")]
    pub complete_date: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    /// Which board originated this sprint. A sprint can belong to
    /// multiple boards via shared filters, but the API always echos
    /// the id we asked from — useful for cache-keying.
    #[serde(default, rename = "originBoardId")]
    pub origin_board_id: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
struct SprintListResponse {
    #[serde(default)]
    values: Vec<Sprint>,
}

impl Sprint {
    /// Sort into the order the picker renders: active first (there is
    /// usually one, sometimes multiple in parallel-track setups), then
    /// future sprints by startDate ascending (soonest-up first), then
    /// closed sprints by completeDate descending (most-recently-closed
    /// first) — capped at `last_n_closed` so the picker doesn't drown
    /// in years-old sprints.
    pub fn sort_for_picker(mut sprints: Vec<Sprint>, last_n_closed: usize) -> Vec<Sprint> {
        let bucket = |s: &Sprint| -> u8 {
            match s.state.to_ascii_lowercase().as_str() {
                "active" => 0,
                "future" => 1,
                _ => 2, // "closed" and anything unexpected
            }
        };
        sprints.sort_by(|a, b| {
            let ba = bucket(a);
            let bb = bucket(b);
            if ba != bb {
                return ba.cmp(&bb);
            }
            match ba {
                // Active: startDate asc, missing dates last.
                0 => cmp_iso_opt_asc(a.start_date.as_deref(), b.start_date.as_deref())
                    .then_with(|| a.name.cmp(&b.name)),
                // Future: startDate asc, missing dates last.
                1 => cmp_iso_opt_asc(a.start_date.as_deref(), b.start_date.as_deref())
                    .then_with(|| a.name.cmp(&b.name)),
                // Closed: completeDate desc (most-recent first), fall
                // back to endDate desc, then name desc.
                _ => {
                    let a_c = a.complete_date.as_deref().or(a.end_date.as_deref());
                    let b_c = b.complete_date.as_deref().or(b.end_date.as_deref());
                    cmp_iso_opt_desc(a_c, b_c).then_with(|| b.name.cmp(&a.name))
                }
            }
        });
        // Trim trailing closed sprints past the cap. Active + future
        // are always kept in full.
        let mut kept_closed = 0usize;
        sprints.retain(|s| {
            if s.state.eq_ignore_ascii_case("closed") {
                kept_closed += 1;
                kept_closed <= last_n_closed
            } else {
                true
            }
        });
        sprints
    }
}

fn cmp_iso_opt_asc(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn cmp_iso_opt_desc(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// 2026-08-17 (task #893) — one of a board's saved "quick filters".
/// Each is a named JQL fragment (`assignee = currentUser()`,
/// `labels = "hotfix"`, etc.) that Jira Cloud's board toolbar
/// surfaces as a toggleable chip. Layered into the board fetch via
/// `?jql=<jql>` so an active-quick-filter selection narrows what the
/// board returns.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct QuickFilter {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub jql: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Board that owns this quick filter. Present but redundant for
    /// our use (we only ever ask for one board at a time).
    #[serde(default, rename = "boardId")]
    pub board_id: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
struct QuickFilterListResponse {
    #[serde(default)]
    values: Vec<QuickFilter>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct ProjectVersion {
    pub name: String,
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, rename = "startDate")]
    pub start_date: Option<String>,
    /// Kept for future "release date" column / filter; not yet used.
    #[serde(default, rename = "releaseDate")]
    pub release_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adf_to_text_extracts_paragraph_text() {
        let doc = json!({
            "type": "doc",
            "content": [
                {
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": "hello world" }]
                }
            ]
        });
        let out = adf_to_text(&doc);
        assert_eq!(out.trim(), "hello world");
    }

    #[test]
    fn adf_to_text_joins_multiple_paragraphs_with_newlines() {
        let doc = json!({
            "type": "doc",
            "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "first" }] },
                { "type": "paragraph", "content": [{ "type": "text", "text": "second" }] }
            ]
        });
        let out = adf_to_text(&doc);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn adf_to_text_walks_nested_marks_and_inline() {
        let doc = json!({
            "type": "paragraph",
            "content": [
                { "type": "text", "text": "bold ", "marks": [{ "type": "strong" }] },
                { "type": "text", "text": "and " },
                { "type": "text", "text": "italic", "marks": [{ "type": "em" }] }
            ]
        });
        let out = adf_to_text(&doc);
        assert_eq!(out.trim(), "bold and italic");
    }

    #[test]
    fn adf_to_text_handles_bullet_list() {
        let doc = json!({
            "type": "bulletList",
            "content": [
                {
                    "type": "listItem",
                    "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "one" }] }
                    ]
                },
                {
                    "type": "listItem",
                    "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "two" }] }
                    ]
                }
            ]
        });
        let out = adf_to_text(&doc);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn adf_to_text_on_empty_doc_returns_empty() {
        let doc = json!({});
        assert_eq!(adf_to_text(&doc), "");
    }

    #[test]
    fn plain_to_adf_wraps_single_line() {
        let doc = plain_to_adf("hello");
        assert_eq!(doc["type"], "doc");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["content"][0]["type"], "paragraph");
        assert_eq!(doc["content"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn plain_to_adf_one_paragraph_per_line() {
        let doc = plain_to_adf("first\nsecond\nthird");
        let paragraphs = doc["content"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0]["content"][0]["text"], "first");
        assert_eq!(paragraphs[1]["content"][0]["text"], "second");
        assert_eq!(paragraphs[2]["content"][0]["text"], "third");
    }

    #[test]
    fn plain_to_adf_blank_lines_become_empty_paragraphs() {
        let doc = plain_to_adf("a\n\nb");
        let paragraphs = doc["content"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[1]["type"], "paragraph");
        assert!(paragraphs[1].get("content").is_none());
    }

    fn mk_sprint(id: u64, state: &str, start: Option<&str>, end: Option<&str>) -> Sprint {
        Sprint {
            id,
            name: format!("Sprint {id}"),
            state: state.into(),
            start_date: start.map(|s| s.into()),
            end_date: end.map(|s| s.into()),
            complete_date: end.map(|s| s.into()),
            goal: None,
            origin_board_id: Some(1),
        }
    }

    #[test]
    fn sprint_picker_sort_puts_active_first_then_future_then_closed_recent() {
        let sprints = vec![
            mk_sprint(4, "closed", Some("2026-06-01"), Some("2026-06-14")),
            mk_sprint(1, "closed", Some("2026-05-01"), Some("2026-05-14")),
            mk_sprint(6, "future", Some("2026-08-15"), None),
            mk_sprint(5, "future", Some("2026-08-01"), None),
            mk_sprint(9, "active", Some("2026-07-15"), Some("2026-07-29")),
        ];
        let sorted = Sprint::sort_for_picker(sprints, 3);
        let ids: Vec<u64> = sorted.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![9, 5, 6, 4, 1]);
    }

    #[test]
    fn sprint_picker_sort_caps_closed_at_n() {
        let sprints = vec![
            mk_sprint(1, "closed", Some("2026-05-01"), Some("2026-05-14")),
            mk_sprint(2, "closed", Some("2026-05-15"), Some("2026-05-28")),
            mk_sprint(3, "closed", Some("2026-06-01"), Some("2026-06-14")),
            mk_sprint(4, "closed", Some("2026-06-15"), Some("2026-06-28")),
            mk_sprint(5, "closed", Some("2026-07-01"), Some("2026-07-14")),
        ];
        let sorted = Sprint::sort_for_picker(sprints, 3);
        assert_eq!(sorted.len(), 3, "kept only last 3 closed");
        let ids: Vec<u64> = sorted.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![5, 4, 3], "most-recently-closed first");
    }

    #[test]
    fn sprint_picker_sort_active_and_future_always_kept_regardless_of_cap() {
        let sprints = vec![
            mk_sprint(1, "closed", Some("2026-05-01"), Some("2026-05-14")),
            mk_sprint(2, "closed", Some("2026-05-15"), Some("2026-05-28")),
            mk_sprint(3, "active", Some("2026-07-15"), Some("2026-07-29")),
            mk_sprint(4, "future", Some("2026-08-01"), None),
            mk_sprint(5, "future", Some("2026-08-15"), None),
        ];
        let sorted = Sprint::sort_for_picker(sprints, 0);
        let ids: Vec<u64> = sorted.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![3, 4, 5], "closed dropped, active + future kept");
    }

    #[test]
    fn sprint_picker_sort_missing_start_dates_sink_within_bucket() {
        let mut a = mk_sprint(1, "future", None, None);
        a.name = "no-date".into();
        let b = mk_sprint(2, "future", Some("2026-08-01"), None);
        let c = mk_sprint(3, "future", Some("2026-08-15"), None);
        let sorted = Sprint::sort_for_picker(vec![a, b, c], 3);
        let ids: Vec<u64> = sorted.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    #[test]
    fn plain_to_adf_then_adf_to_text_round_trips() {
        let original = "hello world\nsecond line\nthird";
        let doc = plain_to_adf(original);
        let back = adf_to_text(&doc);
        // adf_to_text emits a trailing newline after each block;
        // trim for comparison.
        assert_eq!(back.trim_end(), original);
    }
}
