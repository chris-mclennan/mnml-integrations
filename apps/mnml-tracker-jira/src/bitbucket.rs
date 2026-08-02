//! Minimal Bitbucket Cloud REST API v2 client — just the sub-flow
//! needed to expand a merged linked PR into its post-merge pipeline.
//!
//! Not intended as a general-purpose Bitbucket client; the full
//! surface lives in the sibling `mnml-forge-bitbucket`. This file
//! only covers:
//!
//!   1. Reading `BITBUCKET_ACCESS_TOKEN` from env (Bearer auth for
//!      repository/workspace access tokens).
//!   2. Parsing `https://bitbucket.org/{ws}/{repo}/pull-requests/{id}`
//!      URLs into their three parts (workspace, repo, pr_id).
//!   3. Fetching the PR's `merge_commit.hash` (present on merged PRs
//!      only).
//!   4. Listing recent pipelines and client-side-filtering by commit
//!      hash — same trick the Bitbucket sibling uses since
//!      `?target.commit.hash=` is unreliable on that endpoint.
//!
//! Base URL: <https://api.bitbucket.org/2.0>.
//! Auth: `Authorization: Bearer <token>`. Repository/workspace
//! access tokens (created in Bitbucket → Repository settings →
//! Access tokens) accept this form; classic app passwords do NOT.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

const BASE: &str = "https://api.bitbucket.org/2.0";

/// Minimal Bitbucket Cloud client — enough for the pipeline sub-flow.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    auth_header: String,
}

impl Client {
    /// Build a client from the `BITBUCKET_ACCESS_TOKEN` env var.
    /// Returns `Err` when the var is missing or empty so the UI can
    /// render a clean "pipeline lookup failed: …" hint next to the
    /// expanded PR instead of blowing up.
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("BITBUCKET_ACCESS_TOKEN")
            .ok()
            .and_then(|s| {
                let s = s.trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            })
            .ok_or_else(|| {
                anyhow!("BITBUCKET_ACCESS_TOKEN not set — needed to fetch post-merge pipelines")
            })?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("mnml-tracker-jira/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            auth_header: format!("Bearer {token}"),
        })
    }

    /// Fetch the merge commit hash for a PR by (workspace, repo, id).
    /// Returns `Ok(None)` for PRs that aren't merged (no
    /// `merge_commit` field on the response); `Err` on transport /
    /// non-2xx failures.
    pub async fn fetch_pr_merge_commit(
        &self,
        workspace: &str,
        repo: &str,
        pr_id: &str,
    ) -> Result<Option<String>> {
        let url = format!("{BASE}/repositories/{workspace}/{repo}/pullrequests/{pr_id}");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .context("bitbucket PR detail fetch failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bitbucket PR detail {status}: {text}"));
        }
        let body: PrDetail = resp.json().await.context("parsing PR detail response")?;
        let hash = body.merge_commit.and_then(|c| {
            if c.hash.is_empty() {
                None
            } else {
                Some(c.hash)
            }
        });
        Ok(hash)
    }

    /// Recent pipelines for a repo, newest-first. Ported from the
    /// `mnml-forge-bitbucket` sibling; kept minimal here because the
    /// only caller is `list_pipelines_by_commit` below.
    async fn list_recent_pipelines(
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

    /// Pipelines that ran on a specific commit SHA, newest-first.
    ///
    /// Impl note (verbatim from the `mnml-forge-bitbucket` sibling):
    /// Bitbucket's pipelines endpoint does NOT reliably honor
    /// `?target.commit.hash=` as a query filter — passing it either
    /// returns everything unfiltered or (empirically) an empty
    /// list. Instead we fetch a batch of the most-recent pipelines
    /// and filter client-side. Also — the PR API returns
    /// `merge_commit.hash` as a 12-char short SHA, while the
    /// pipelines API returns full 40-char SHAs. Match by "does one
    /// start with the other" (both directions, both lowercase).
    pub async fn list_pipelines_by_commit(
        &self,
        workspace: &str,
        repo: &str,
        commit_hash: &str,
    ) -> Result<Vec<Pipeline>> {
        let all = self.list_recent_pipelines(workspace, repo, 60).await?;
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

    /// Combined helper: parse a PR URL → fetch its merge_commit →
    /// list pipelines for that commit. Returns:
    ///
    ///   * `Ok(vec![...])` on success (may be empty if no pipeline
    ///     ran on the merge commit — the UI renders "no pipeline
    ///     ran on merge commit" in that case).
    ///   * `Err(...)` when the URL isn't parseable (non-Bitbucket
    ///     host / wrong shape), when the PR isn't merged (no
    ///     merge_commit), or when a Bitbucket request fails. The
    ///     UI shows the error message verbatim.
    pub async fn fetch_pipelines_for_pr_url(&self, pr_url: &str) -> Result<Vec<Pipeline>> {
        let Some((ws, repo, pr_id)) = parse_pr_url(pr_url) else {
            // TODO: GitHub — parse `github.com/{owner}/{repo}/pull/{n}`
            // once a repo with a linked GitHub PR shows up. For now
            // any non-Bitbucket URL trips this branch and the UI
            // renders the message next to the PR row.
            if pr_url.contains("github.com") {
                return Err(anyhow!("GitHub PR pipeline lookup not supported yet"));
            }
            return Err(anyhow!("not a bitbucket PR URL"));
        };
        let Some(hash) = self.fetch_pr_merge_commit(&ws, &repo, &pr_id).await? else {
            return Err(anyhow!("PR not merged — no merge commit"));
        };
        self.list_pipelines_by_commit(&ws, &repo, &hash).await
    }
}

/// Extract (workspace, repo, pr_id) from a Bitbucket PR URL. Returns
/// `None` when the URL isn't a `bitbucket.org/{ws}/{repo}/pull-requests/{id}`
/// shape. Case-sensitive on `bitbucket.org` (Bitbucket serves that
/// domain lowercased); the path segments are passed through
/// unmodified.
pub fn parse_pr_url(url: &str) -> Option<(String, String, String)> {
    let after_scheme = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let mut parts = after_scheme.splitn(2, '/');
    let host = parts.next()?;
    if !host.eq_ignore_ascii_case("bitbucket.org") {
        return None;
    }
    let rest = parts.next()?;
    // Expect exactly {ws}/{repo}/pull-requests/{id}[/rest]. Split on
    // '/' and pluck the four segments; a mismatched separator or
    // missing segment ⇒ None.
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() < 4 {
        return None;
    }
    if segments[2] != "pull-requests" {
        return None;
    }
    let ws = segments[0];
    let repo = segments[1];
    // Bitbucket sometimes writes `pull-requests/1234/diff` etc — the
    // id is always the fourth segment. Strip any trailing chars past
    // the numeric prefix so anchors / suffixes don't break the fetch.
    let id_raw = segments[3];
    let id: String = id_raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    if id.is_empty() || ws.is_empty() || repo.is_empty() {
        return None;
    }
    Some((ws.to_string(), repo.to_string(), id))
}

// ─── Response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PrDetail {
    #[serde(default)]
    merge_commit: Option<CommitRef>,
}

#[derive(Debug, Deserialize)]
struct PipelinePage {
    #[serde(default)]
    values: Vec<Pipeline>,
}

/// One Bitbucket pipeline (build) record. Fields mirror the sibling's
/// shape closely enough to render — only what the UI reads is kept
/// pub; helper methods land on `impl` blocks below.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Pipeline {
    pub uuid: String,
    #[serde(default)]
    pub build_number: i64,
    #[serde(default)]
    pub state: Option<PipelineState>,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub duration_in_seconds: Option<i64>,
    #[serde(default)]
    pub target: Option<PipelineTarget>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineState {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub result: Option<PipelineStateResult>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineStateResult {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct PipelineTarget {
    #[serde(default)]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub commit: Option<CommitRef>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct CommitRef {
    #[serde(default)]
    pub hash: String,
}

impl Pipeline {
    /// Best-guess label for the pipeline's outcome. Collapses
    /// COMPLETED → result.name so a finished pipeline reads as
    /// `SUCCESSFUL` / `FAILED` / `STOPPED` rather than the less
    /// informative lifecycle name. Falls back to the top-level state
    /// name (`PENDING` / `IN_PROGRESS` / etc) otherwise.
    pub fn state_label(&self) -> &str {
        match self.state.as_ref() {
            Some(s) if !s.name.is_empty() => {
                if let Some(r) = s.result.as_ref()
                    && !r.name.is_empty()
                {
                    return r.name.as_str();
                }
                s.name.as_str()
            }
            _ => "UNKNOWN",
        }
    }

    /// Target branch name (`main`, `develop`, `release/…`), or `—`
    /// when the pipeline was triggered by something without a
    /// `ref_name` (custom / manual).
    pub fn branch_label(&self) -> &str {
        self.target
            .as_ref()
            .and_then(|t| t.ref_name.as_deref())
            .unwrap_or("—")
    }

    /// Just the date portion (`YYYY-MM-DD`) of `created_on`. String-
    /// sliced rather than parsed to avoid pulling chrono into this
    /// module (matches the Bitbucket sibling's convention).
    pub fn created_date(&self) -> String {
        self.created_on
            .as_deref()
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default()
    }

    /// Human-friendly duration — e.g. `3m 45s`, `12s`, or `—` when
    /// missing.
    pub fn duration_label(&self) -> String {
        match self.duration_in_seconds {
            Some(s) if s > 0 => {
                let m = s / 60;
                let r = s % 60;
                if m > 0 {
                    format!("{m}m {r:02}s")
                } else {
                    format!("{r}s")
                }
            }
            _ => "—".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_url_extracts_workspace_repo_and_id() {
        let out = parse_pr_url("https://bitbucket.org/tattle/tattle-api/pull-requests/2023");
        assert_eq!(
            out,
            Some((
                "tattle".to_string(),
                "tattle-api".to_string(),
                "2023".to_string()
            ))
        );
    }

    #[test]
    fn parse_pr_url_tolerates_trailing_path_and_query() {
        // Bitbucket sometimes appends `/diff` or `#comment-N` — the
        // id parse should skip anything past the numeric prefix.
        let out = parse_pr_url("https://bitbucket.org/foo/bar/pull-requests/42/diff");
        assert_eq!(
            out,
            Some(("foo".to_string(), "bar".to_string(), "42".to_string()))
        );
    }

    #[test]
    fn parse_pr_url_returns_none_for_github() {
        assert_eq!(parse_pr_url("https://github.com/foo/bar/pull/1"), None);
    }

    #[test]
    fn parse_pr_url_returns_none_for_non_pr_bitbucket_url() {
        assert_eq!(
            parse_pr_url("https://bitbucket.org/foo/bar/commits/abc123"),
            None
        );
    }

    #[test]
    fn parse_pr_url_returns_none_for_missing_id() {
        assert_eq!(
            parse_pr_url("https://bitbucket.org/foo/bar/pull-requests/"),
            None
        );
    }

    #[test]
    fn parse_pr_url_accepts_url_without_scheme() {
        // No scheme — trim_start_matches no-ops and the rest of the
        // parse still fires. Handy for tolerating pasted URLs that
        // lost their `https://` (Bitbucket's own copy button
        // sometimes yields the bare form).
        let out = parse_pr_url("bitbucket.org/foo/bar/pull-requests/9");
        assert_eq!(
            out,
            Some(("foo".to_string(), "bar".to_string(), "9".to_string()))
        );
    }

    #[test]
    fn pipeline_state_label_prefers_result_over_lifecycle() {
        let p = Pipeline {
            uuid: "u".into(),
            build_number: 1,
            state: Some(PipelineState {
                name: "COMPLETED".into(),
                result: Some(PipelineStateResult {
                    name: "SUCCESSFUL".into(),
                }),
            }),
            created_on: None,
            duration_in_seconds: None,
            target: None,
        };
        assert_eq!(p.state_label(), "SUCCESSFUL");
    }

    #[test]
    fn pipeline_state_label_falls_back_to_lifecycle_when_no_result() {
        let p = Pipeline {
            uuid: "u".into(),
            build_number: 1,
            state: Some(PipelineState {
                name: "IN_PROGRESS".into(),
                result: None,
            }),
            created_on: None,
            duration_in_seconds: None,
            target: None,
        };
        assert_eq!(p.state_label(), "IN_PROGRESS");
    }

    #[test]
    fn pipeline_duration_label_formats_minutes_and_seconds() {
        let mut p = Pipeline {
            uuid: "u".into(),
            build_number: 1,
            state: None,
            created_on: None,
            duration_in_seconds: Some(225),
            target: None,
        };
        assert_eq!(p.duration_label(), "3m 45s");
        p.duration_in_seconds = Some(12);
        assert_eq!(p.duration_label(), "12s");
        p.duration_in_seconds = None;
        assert_eq!(p.duration_label(), "—");
    }

    #[test]
    fn pipeline_created_date_slices_first_ten_chars() {
        let p = Pipeline {
            uuid: "u".into(),
            build_number: 1,
            state: None,
            created_on: Some("2026-07-29T10:23:11.000+0000".into()),
            duration_in_seconds: None,
            target: None,
        };
        assert_eq!(p.created_date(), "2026-07-29");
    }
}
