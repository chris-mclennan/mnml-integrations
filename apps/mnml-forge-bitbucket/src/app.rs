//! App state — what's loaded, what's selected, the configured query
//! for each tab.

use crate::bitbucket::{BranchRef, Client, Comment, Pipeline, PullRequest, RepoPipelines, RepoPrs};
use crate::config::{Config, Tab};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// One row for `tree_focused_row`'s uniform tree walk — a repo slug
/// plus the child labels visible under it when expanded (branch names
/// for `RepoTree`, `"PR #N"` synthetics for `RepoPrTree`).
type TreeRow = (String, Vec<String>);

/// Type-erased iterator over `TreeRow`s so `tree_focused_row` can
/// walk both `RepoTree` and `RepoPrTree` through a single loop.
type BoxedTreeRowsIter<'a> = Box<dyn Iterator<Item = TreeRow> + 'a>;

/// Canonical trunk / release / integration branch names, in the
/// order we want them shown. Case-insensitive match. Anything not on
/// this list is a "feature" branch. tree-redesign 2026-07-19 —
/// mirrors mnml-aws-amplify's environment-first layout so the user
/// sees `main → develop → staging → prod` at the top of every repo
/// instead of whichever branch happened to have the last commit.
const MAJOR_BRANCH_ORDER: &[&str] = &[
    "main",
    "master",
    "trunk",
    "develop",
    "dev",
    "staging",
    "stage",
    "beta",
    "production",
    "prod",
    "release",
    "hotfix",
];

/// Return the priority rank of a branch name (lower = higher up in
/// the list); `None` = not a major branch. Match is case-insensitive
/// and prefix-based for `release/…` / `hotfix/…` families.
fn major_rank(name: &str) -> Option<usize> {
    let lower = name.to_lowercase();
    for (i, &m) in MAJOR_BRANCH_ORDER.iter().enumerate() {
        if lower == m || lower.starts_with(&format!("{m}/")) {
            return Some(i);
        }
    }
    None
}

/// Curate a repo's fetched branch list down to `(all major branches
/// present) + (the single most-recent feature branch)`. Majors sort
/// by their canonical order; the feature is picked by
/// latest_pipeline.created_on desc (falling back to the input order
/// when timestamps are absent — which matches the API's
/// `-target.date` sort).
/// How many branches to keep per prefix-family major (`release/*`,
/// `hotfix/*`, etc.). Literal-name majors (main / master / develop /
/// staging / prod / beta) always keep their single branch. Family
/// caps prevent a repo with 40 `release/x.y` tags from swamping the
/// per-repo row. 2026-07-20 — cap 1 per user ask "only most recent
/// one is important". Users can jump to the full branches page on
/// web with `o` on the repo header row.
const MAX_PER_FAMILY: usize = 1;

/// Drop feature + release + hotfix branches whose latest activity is
/// older than this many days. Keeps literal-name majors (main /
/// master / trunk / develop / dev / staging / stage / beta /
/// production / prod) always — those are eternal branches that
/// legitimately stay quiet between deployments. 2026-07-20 — set
/// to two weeks per user: "2 weeks not 45 days". Matches typical
/// sprint length so anything from a completed sprint drops off.
const STALE_AFTER_DAYS: i64 = 14;

/// Branches whose id is one of the always-keep literal names,
/// regardless of age. Prefix-family majors (release/*, hotfix/*)
/// are NOT in this set — those go stale like features.
fn is_eternal_major(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "main"
            | "master"
            | "trunk"
            | "develop"
            | "dev"
            | "staging"
            | "stage"
            | "beta"
            | "production"
            | "prod"
    )
}

/// Return the number of days between an ISO-8601 timestamp and now.
/// `None` if the timestamp is missing or unparseable — the caller
/// treats that as "unknown age, keep it" so we err on the side of
/// showing rather than hiding.
fn days_since(iso: &str) -> Option<i64> {
    // Minimal parser — bitbucket sends `2026-07-20T12:34:56.789Z`
    // or `+00:00`. We only need the date portion for day math.
    let date_part = iso.split('T').next()?;
    let mut parts = date_part.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    // Julian day for date.
    let jd_of = |y: i32, m: u32, d: u32| -> i64 {
        let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
        let a = y / 100;
        let b = 2 - a + a / 4;
        (365.25 * (y as f64 + 4716.0)) as i64
            + (30.6001 * (m as f64 + 1.0)) as i64
            + d as i64
            + b as i64
            - 1524
    };
    // Compute "today" from the system clock without touching chrono
    // — use SystemTime::now converted to a naive UTC date.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    // Unix epoch = 1970-01-01. JD of 1970-01-01 = 2440588.
    let today_jd = 2440588 + now / 86_400;
    Some(today_jd - jd_of(y, m, d))
}

fn curate_branches(
    branches: Vec<crate::bitbucket::BranchWithPipeline>,
) -> Vec<crate::bitbucket::BranchWithPipeline> {
    // Staleness filter — drop features + release + hotfix that
    // haven't seen activity in STALE_AFTER_DAYS. Literal-name
    // majors (main/master/develop/staging/prod/beta) never go
    // stale via this filter. Undated branches (no pipeline + no
    // parsed target date) are kept — err on the side of showing.
    let branches: Vec<_> = branches
        .into_iter()
        .filter(|b| {
            if is_eternal_major(&b.name) {
                return true;
            }
            match b.last_activity_on.as_deref().and_then(days_since) {
                Some(days) => days <= STALE_AFTER_DAYS,
                None => true,
            }
        })
        .collect();
    // Group each major branch by its family rank so we can cap
    // prefix-families (release/*, hotfix/*) without touching the
    // one-and-only main / master / develop / etc rows.
    let mut buckets: std::collections::BTreeMap<usize, Vec<crate::bitbucket::BranchWithPipeline>> =
        std::collections::BTreeMap::new();
    let mut features: Vec<crate::bitbucket::BranchWithPipeline> = Vec::new();
    for b in branches {
        match major_rank(&b.name) {
            Some(rank) => buckets.entry(rank).or_default().push(b),
            None => features.push(b),
        }
    }
    // Within each bucket, sort by pipeline recency (newest first)
    // so the cap keeps the "most active" branches for that family.
    // For literal-name majors there's usually exactly one; the sort
    // is a no-op there but keeps the code branchless.
    let mut out: Vec<crate::bitbucket::BranchWithPipeline> = Vec::new();
    for (_rank, mut group) in buckets {
        group.sort_by(|a, b| {
            let ka = a
                .latest_pipeline
                .as_ref()
                .and_then(|p| p.created_on.clone());
            let kb = b
                .latest_pipeline
                .as_ref()
                .and_then(|p| p.created_on.clone());
            kb.cmp(&ka)
        });
        group.truncate(MAX_PER_FAMILY);
        out.extend(group);
    }
    // Plus one feature branch (most-recent pipeline; fall back to
    // API's `-target.date` #1 when no pipelines).
    let top_feature = features.into_iter().max_by(|a, b| {
        let ka = a
            .latest_pipeline
            .as_ref()
            .and_then(|p| p.created_on.clone());
        let kb = b
            .latest_pipeline
            .as_ref()
            .and_then(|p| p.created_on.clone());
        ka.cmp(&kb)
    });
    if let Some(f) = top_feature {
        out.push(f);
    }
    out
}

/// Per-tab content kind. The PR / Pipeline / Branch dispatch lives on
/// `TabKind` rather than a bare string so the refresh + render paths
/// can exhaustively match.
///
/// tree-redesign 2026-07-14 phase 2b — added three workspace-wide
/// variants (WorkspaceOpenPRs / WorkspaceMergedPRs / WorkspacePipelines)
/// that fan out across every repo in the configured `scope`. Legacy
/// PullRequests / Pipelines / Branches unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    // Legacy per-repo kinds.
    PullRequests,
    Pipelines,
    Branches,
    // Workspace-wide kinds (phase 2b).
    WorkspaceOpenPRs,
    WorkspaceMergedPRs,
    WorkspacePipelines,
}

impl TabKind {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "pull_requests" => Ok(Self::PullRequests),
            "pipelines" => Ok(Self::Pipelines),
            "branches" => Ok(Self::Branches),
            "workspace_open_prs" => Ok(Self::WorkspaceOpenPRs),
            "workspace_merged_prs" => Ok(Self::WorkspaceMergedPRs),
            "workspace_pipelines" => Ok(Self::WorkspacePipelines),
            other => Err(anyhow::anyhow!("unknown tab kind: {other}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PullRequests => "pull_requests",
            Self::Pipelines => "pipelines",
            Self::Branches => "branches",
            Self::WorkspaceOpenPRs => "workspace_open_prs",
            Self::WorkspaceMergedPRs => "workspace_merged_prs",
            Self::WorkspacePipelines => "workspace_pipelines",
        }
    }

    /// True when the tab derives its scope from top-level config
    /// (`Config::scope` / `hidden_repos` / `explicit_repos`) rather
    /// than from a per-tab `repo` field. Used at resolve-time to
    /// skip the `repo` requirement + at refresh-time to route to
    /// the workspace-wide fetch helpers.
    ///
    /// Kept as public API for downstream consumers even though the
    /// in-tree callers were inlined into TabSpec::resolve — the
    /// predicate is the canonical way to answer "does this tab kind
    /// need a per-tab repo?".
    #[allow(dead_code)]
    pub fn is_workspace_wide(self) -> bool {
        matches!(
            self,
            Self::WorkspaceOpenPRs | Self::WorkspaceMergedPRs | Self::WorkspacePipelines
        )
    }
}

/// Loaded data for a tab — variant determined by the resolved `TabKind`.
///
/// tree-redesign 2026-07-14 phase 2b — added `RepoTree` for the
/// workspace_pipelines tab. WorkspaceOpenPRs / WorkspaceMergedPRs
/// reuse the existing `PullRequests` variant (same shape — the
/// only difference is which fetch path populates it).
#[derive(Debug, Clone)]
pub enum TabData {
    PullRequests(Vec<PullRequest>),
    Pipelines(Vec<Pipeline>),
    Branches(Vec<BranchRef>),
    /// Amplify-style repo tree: one row per repo, each expandable
    /// to reveal its branches with per-branch pipeline status.
    /// `expanded` holds the slugs of currently-open rows so
    /// re-render preserves state across selection moves.
    RepoTree {
        rows: Vec<RepoPipelines>,
        expanded: HashSet<String>,
    },
    /// Same tree shape as `RepoTree` but each repo expands to its
    /// PRs (with per-PR state / author / branch / date columns)
    /// instead of pipeline branches. Powers the workspace_open_prs
    /// + workspace_merged_prs tabs after the user asked for
    ///   per-repo drill-down on those (2026-07-15). Shares the
    ///   `tree_*` navigation helpers below with RepoTree via the
    ///   generic slug/child-count shape.
    RepoPrTree {
        rows: Vec<RepoPrs>,
        expanded: HashSet<String>,
        /// 2026-07-24 — false = filter PRs to those updated in the
        /// last RECENT_WINDOW_HOURS; true = show every fetched PR.
        /// Toggled by clicking the synthetic "[ Show N older PRs ]"
        /// footer row that appears when the filter has hidden any
        /// PRs. Reset to false on every refresh so the recency
        /// filter re-applies against the fresh fetch.
        show_all: bool,
    },
}

impl TabData {
    pub fn empty_for(kind: TabKind) -> Self {
        match kind {
            // Legacy per-repo PR tab stays flat.
            TabKind::PullRequests => Self::PullRequests(Vec::new()),
            // New workspace-wide PR tabs are per-repo trees
            // (2026-07-15 user request).
            TabKind::WorkspaceOpenPRs | TabKind::WorkspaceMergedPRs => Self::RepoPrTree {
                rows: Vec::new(),
                expanded: HashSet::new(),
                show_all: false,
            },
            TabKind::Pipelines => Self::Pipelines(Vec::new()),
            TabKind::Branches => Self::Branches(Vec::new()),
            TabKind::WorkspacePipelines => Self::RepoTree {
                rows: Vec::new(),
                expanded: HashSet::new(),
            },
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::PullRequests(v) => v.len(),
            Self::Pipelines(v) => v.len(),
            Self::Branches(v) => v.len(),
            // Tree "length" is the count of VISIBLE rows (repo
            // rows always + child rows for each expanded repo).
            // Renderers use this to clamp cursor navigation.
            Self::RepoTree { rows, expanded } => rows
                .iter()
                .map(|r| {
                    1 + if expanded.contains(&r.slug) {
                        r.branches.len()
                    } else {
                        0
                    }
                })
                .sum(),
            Self::RepoPrTree {
                rows,
                expanded,
                show_all,
            } => {
                // 2026-07-24 — visible-row count. When show_all is
                // false and any PR falls outside the 24-hour recency
                // window, we count it as HIDDEN and reserve one
                // extra row at the end for the "[ Show N older
                // PRs ]" footer. When show_all is true (or nothing
                // is hidden) the footer isn't rendered.
                let mut total = 0usize;
                let mut hidden = 0usize;
                for r in rows {
                    total += 1; // repo header row
                    if expanded.contains(&r.slug) {
                        let (vis, hid) = count_recent_prs(&r.prs, *show_all);
                        total += vis;
                        hidden += hid;
                    }
                }
                if !show_all && hidden > 0 {
                    total += 1; // footer
                }
                total
            }
        }
    }

    /// 2026-07-24 — for the "[ Show N older PRs ]" footer row: how
    /// many PRs are currently hidden by the 24-hour recency filter,
    /// summed across all EXPANDED repos in a RepoPrTree tab. `None`
    /// on non-RepoPrTree data or when nothing is hidden (in which
    /// case the footer isn't rendered).
    pub fn hidden_pr_count(&self) -> Option<usize> {
        if let Self::RepoPrTree {
            rows,
            expanded,
            show_all,
        } = self
        {
            if *show_all {
                return None;
            }
            let mut hid = 0usize;
            for r in rows {
                if expanded.contains(&r.slug) {
                    hid += count_recent_prs(&r.prs, false).1;
                }
            }
            if hid == 0 { None } else { Some(hid) }
        } else {
            None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct App {
    pub cfg: Config,
    pub client: Client,
    /// Authenticated user's account_id, resolved at startup. Drives
    /// the approve/unapprove toggle + the "✓ approved by you" badge.
    /// `None` ⇒ no Account:Read scope or whoami failed.
    pub me_account_id: Option<String>,
    /// #1103 f/u6 (2026-08-20) — auth user's display name, cached
    /// at startup from whoami. Used by the filter toolbar to label
    /// the Author chip when the tab is mine-filtered.
    pub me_display_name: Option<String>,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// Right-half detail panel visibility (toggled with `d`).
    /// Only meaningful on `PullRequests` tabs in v0.3 — other kinds
    /// render a brief "no detail panel for this view" message.
    pub details_visible: bool,
    /// First-line offset into the detail body (`Ctrl+U/D` scroll).
    pub details_scroll: u16,
    /// Per-PR detail cache, keyed by (workspace, repo, id). Survives
    /// arrow-key navigation so re-selecting a focused row doesn't
    /// re-fetch.
    pub detail_cache: HashMap<(String, String, i64), DetailEntry>,
    /// In-flight detail key (so we don't fire a second fetch on top of
    /// a pending one). `None` when idle.
    pub detail_in_flight: Option<(String, String, i64)>,
    /// Cached resolved scope — the workspace's repo slugs after
    /// applying `Config::scope` / `hidden_repos` / `explicit_repos`
    /// / `recent_window_days` / `repo_order`. Fetched once on
    /// first workspace-wide tab refresh and re-used until a scope
    /// change (`A` / `R` / `E` toggle) or explicit rescan. `None`
    /// = not yet fetched. Keeping this on App (not per-tab) so
    /// three sibling workspace tabs share one API round-trip.
    /// tree-redesign 2026-07-14.
    pub scope_repos: Option<Vec<String>>,
    /// Set by `main.rs` when the sibling was launched with `--only`.
    /// Suppresses the top tab strip regardless of how many tabs
    /// remain after filtering — the caller (mnml's split Bitbucket
    /// chips) has already picked the view for the user.
    pub hide_tab_strip: bool,
    /// 2026-07-24 (re-add) — MERGED PR rows the user drilled into.
    /// Key = (repo_slug, pr_id). When present, the PR row gets an
    /// expand caret + extra sub-rows showing the post-merge pipeline
    /// that ran on the target branch (main/develop/etc.), sourced
    /// from `pr_pipeline_cache`.
    pub expanded_prs: HashSet<(String, i64)>,
    /// Fetched pipelines for MERGED PRs, keyed by (repo_slug, pr_id).
    /// Populated on first expand via
    /// `Client::list_pipelines_by_commit(ws, repo, merge_commit_hash)`.
    /// Absent = "not fetched yet or in flight"; present = "ready to
    /// render" (empty vec = fetched, no pipeline ran on that commit).
    pub pr_pipeline_cache: HashMap<(String, i64), Vec<Pipeline>>,
    /// #1000 (2026-08-18) — clickable footer chord chips. Rebuilt
    /// every render frame in `ui::draw_status`. Each entry is
    /// `(chip_rect, synthesized_KeyEvent)`; on left-click we route
    /// through the same `keys::handle` → `keys::apply` pipeline as
    /// physical key presses, so mouse and keyboard stay one code
    /// path. Empty until first `draw`.
    pub hint_chip_rects: Vec<(ratatui::layout::Rect, crossterm::event::KeyEvent)>,
    /// #1103 (2026-08-20) — filter toolbar chip rects. Populated by
    /// `draw_filter_toolbar`; consumed by the mouse handler to
    /// route clicks to the right chip action (Status cycle, All =
    /// mine toggle, etc.). Empty until first `draw`.
    pub filter_chip_rects: Vec<(ratatui::layout::Rect, FilterChip)>,
}

/// #1103 (2026-08-20) — filter toolbar chip identity. The PR-family
/// tabs (WorkspaceOpenPRs / WorkspaceMergedPRs / PullRequests) get
/// the PR filter bar (Search/Status/Author/TargetBranch/All);
/// pipeline tabs (Pipelines / WorkspacePipelines) get the pipeline
/// filter bar (Branch/PipelineType/Status/TriggerType). Which chips
/// render is decided in `draw_filter_toolbar` per active tab's
/// `TabKind`. Not every chip is wired yet — see `ui.rs` click
/// routing for current status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterChip {
    // PRs
    Search,
    Status,
    Author,
    TargetBranch,
    // Pipelines
    Branch,
    PipelineType,
    TriggerType,
    /// Pipelines — right-aligned action chips (Bitbucket Cloud shows
    /// these as the primary CTAs on the Pipelines page).
    ActionRunPipeline,
    ActionSchedules,
    ActionCaches,
    ActionUsage,
    /// #1053-analog (2026-08-21) — Refresh chip on the right side of
    /// both PR and Pipeline toolbars, mirroring the Jira Work refresh
    /// chip. Fires `refresh_active`. Shown for every tab kind.
    ActionRefresh,
}

/// Cached PR detail + comments. Fetched lazily on first focus while
/// the detail panel is open.
#[derive(Debug, Clone)]
pub struct DetailEntry {
    pub pr: PullRequest,
    pub comments: Vec<Comment>,
}

pub struct TabState {
    pub name: String,
    /// Resolved per-tab fetch spec, captured at App::new from the
    /// config so the refresh path doesn't have to re-resolve.
    pub spec: TabSpec,
    pub data: TabData,
    pub selected: usize,
    pub last_fetched: Option<std::time::Instant>,
    pub last_error: Option<String>,
}

/// Resolved tab fetch spec — what to send to the bitbucket client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceScope {
    /// User-scoped `mode = "mine"` — hit
    /// `/2.0/pullrequests/{account_id}?state=…` for author-only PRs
    /// across the workspace.
    UserAuthored { account_id: String },
    /// `mode = "reviewing"` — Bitbucket Cloud has no direct
    /// endpoint for "workspace-wide reviewer" queries. Fetch
    /// surfaces this variant so the app can render a clean
    /// "not supported" message instead of a 404.
    UserReviewing,
}

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: TabKind,
    pub workspace: String,
    /// `None` ⇒ workspace-level lookup. Pipelines + Branches
    /// always require Some(repo). When `repo` is None, `scope`
    /// tells us which workspace-wide endpoint (or non-support
    /// error) applies.
    pub repo: Option<String>,
    /// Workspace-scope shape for PR tabs when `repo.is_none()`.
    /// Meaningful only for `kind = PullRequests`.
    pub scope: Option<WorkspaceScope>,
    /// PR state — only meaningful for `kind = PullRequests`.
    pub state: String,
    /// BBQL — only meaningful for `kind = PullRequests`. Kept
    /// alongside `scope` so users can layer custom BBQL on top
    /// of the auto mode (e.g. "mine, but only in a given repo").
    pub q: Option<String>,
    /// #1099 f/u (2026-08-20) — post-fetch filter on `WorkspaceOpen`
    /// / `WorkspaceMerged` PR fetches that drops PRs whose author is
    /// not the auth user. Preserves tree grouping. Set at runtime
    /// by `--only prs-mine` when no config tab has `mode = "mine"`.
    pub mine_only: bool,
}

impl TabSpec {
    /// Resolve a `Tab` config entry against the global default
    /// workspace + the resolved current-user account_id (for `mine`
    /// / `reviewing`). `me_account_id` of `None` is allowed but causes
    /// auto-mode PR tabs to emit an explanatory error rather than
    /// firing a malformed query.
    pub fn resolve(
        tab: &Tab,
        default_workspace: &str,
        me_account_id: Option<&str>,
    ) -> Result<Self> {
        let kind = TabKind::from_str(&tab.kind)?;
        let workspace = tab
            .workspace
            .clone()
            .unwrap_or_else(|| default_workspace.to_string());
        match kind {
            TabKind::PullRequests => {
                let (repo, scope, q) = match tab.mode.as_deref() {
                    Some("mine") => {
                        let aid = me_account_id.ok_or_else(|| {
                            anyhow::anyhow!(
                                "mode=\"mine\" needs Account:Read scope on the app password"
                            )
                        })?;
                        (
                            None,
                            Some(WorkspaceScope::UserAuthored {
                                account_id: aid.to_string(),
                            }),
                            tab.q.clone(),
                        )
                    }
                    Some("reviewing") => {
                        // Reject up front so users see the "not
                        // supported" message before hitting the API.
                        me_account_id.ok_or_else(|| {
                            anyhow::anyhow!(
                                "mode=\"reviewing\" needs Account:Read scope on the app password"
                            )
                        })?;
                        (None, Some(WorkspaceScope::UserReviewing), tab.q.clone())
                    }
                    None => (tab.repo.clone(), None, tab.q.clone()),
                    Some(other) => {
                        return Err(anyhow::anyhow!("unknown tab mode: {other}"));
                    }
                };
                Ok(TabSpec {
                    kind,
                    workspace,
                    repo,
                    scope,
                    state: tab.state.clone(),
                    q,
                    mine_only: tab.mine_only,
                })
            }
            TabKind::Pipelines | TabKind::Branches => {
                let repo = tab.repo.clone().ok_or_else(|| {
                    anyhow::anyhow!("kind = `{}` requires a `repo` field", kind.as_str())
                })?;
                Ok(TabSpec {
                    kind,
                    workspace,
                    repo: Some(repo),
                    scope: None,
                    state: String::new(),
                    q: None,
                    mine_only: false,
                })
            }
            // tree-redesign 2026-07-14 — workspace-wide kinds derive
            // their scope from top-level Config (scope / hidden_repos /
            // explicit_repos), NOT from per-tab fields. Resolve
            // succeeds without validating repo/mode/q.
            TabKind::WorkspaceOpenPRs
            | TabKind::WorkspaceMergedPRs
            | TabKind::WorkspacePipelines => Ok(TabSpec {
                kind,
                workspace,
                repo: None,
                scope: None,
                state: String::new(),
                q: None,
                mine_only: tab.mine_only,
            }),
        }
    }
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        // Resolve current-user account_id once. Failure is non-fatal
        // — non-auto tabs still work; auto-mode PR tabs surface the
        // error on their first refresh.
        let (me_account_id, me_display_name, whoami_err) = match client.whoami().await {
            Ok(u) => {
                let name = if u.display_name.is_empty() {
                    None
                } else {
                    Some(u.display_name)
                };
                (u.account_id, name, None)
            }
            Err(e) => (None, None, Some(e.to_string())),
        };
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let parsed_kind = TabKind::from_str(&t.kind).unwrap_or(TabKind::PullRequests);
            match TabSpec::resolve(t, &cfg.workspace, me_account_id.as_deref()) {
                Ok(spec) => tabs.push(TabState {
                    name: t.name.clone(),
                    data: TabData::empty_for(spec.kind),
                    spec,
                    selected: 0,
                    last_fetched: None,
                    last_error: None,
                }),
                Err(e) => tabs.push(TabState {
                    name: t.name.clone(),
                    spec: TabSpec {
                        kind: parsed_kind,
                        workspace: cfg.workspace.clone(),
                        repo: None,
                        scope: None,
                        state: t.state.clone(),
                        q: None,
                        mine_only: t.mine_only,
                    },
                    data: TabData::empty_for(parsed_kind),
                    selected: 0,
                    last_fetched: None,
                    last_error: Some(e.to_string()),
                }),
            }
        }
        let status = whoami_err
            .as_deref()
            .map(|e| format!("whoami failed: {e}"))
            .unwrap_or_default();
        let mut app = App {
            cfg,
            client,
            me_account_id,
            me_display_name,
            tabs,
            active_tab: 0,
            status,
            details_visible: false,
            details_scroll: 0,
            detail_cache: HashMap::new(),
            detail_in_flight: None,
            scope_repos: None,
            hide_tab_strip: false,
            expanded_prs: HashSet::new(),
            pr_pipeline_cache: HashMap::new(),
            hint_chip_rects: Vec::new(),
            filter_chip_rects: Vec::new(),
        };
        // #1117 (2026-08-21) — prefetch hydration. If mnml's
        // background worker has produced a fresh cache and stamped
        // its path via `MNML_PREFETCH_CACHE_FILE`, seed the tabs'
        // data from it instead of doing the cold startup-prefetch
        // loop. `last_fetched` is stamped so the pane treats
        // hydrated tabs as recently refreshed. Cache misses / stale
        // files / parse errors fall through to the normal
        // cold-fetch path silently — losing hydration is never
        // worse than the cold behavior.
        let hydrated = app.hydrate_from_prefetch_cache();

        // Startup prefetch: refresh EVERY configured tab up front so
        // subsequent 1/2/3 (or click) tab-switches show cached data
        // instantly. Previously only the active tab was fetched at
        // startup; switching to Merged / Pipelines then had to await
        // the full workspace-wide fetch (~20s per tab) and the whole
        // UI froze during the await. The workspace `scope_repos`
        // cache means the second + third tabs share the first tab's
        // repo enum → prefetch cost is ~1 enum + 3 parallel-ish
        // workspace queries.
        //
        // User waits during the initial prefetch — set status so
        // there's visible progress ("prefetching 2/3 · Merged…").
        //
        // Skip the walk entirely when hydration populated every
        // tab; otherwise refresh only the tabs the cache didn't
        // touch (a stale cache with a schema mismatch on one tab
        // still gets a valid render on the other).
        let total_tabs = app.tabs.len();
        for i in 0..total_tabs {
            if hydrated && app.tabs[i].last_fetched.is_some() {
                continue;
            }
            app.active_tab = i;
            app.status = format!(
                "prefetching {}/{} · {}…",
                i + 1,
                total_tabs,
                app.tabs[i].name
            );
            app.refresh_active().await;
        }
        // Restore focus to tab 0. Also regenerate status from tab
        // 0's own state — without this, `app.status` still shows
        // the last-prefetched tab's outcome (`"Merged · 12 PRs"` or
        // worse, its error), which reads as "tab 0 is broken" on
        // first paint.
        app.active_tab = 0;
        app.status = match app.tabs.first() {
            Some(t) if t.last_error.is_some() => {
                format!("{}: {}", t.name, t.last_error.as_deref().unwrap_or(""))
            }
            Some(t) => format!("{} · {} rows", t.name, t.data.len()),
            None => String::new(),
        };
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    /// #1117 (2026-08-21) — try to seed `tabs[i].data` from the
    /// prefetch cache mnml core stamped via `MNML_PREFETCH_CACHE_FILE`.
    /// Returns `true` iff at least one tab was populated (in which
    /// case the startup-prefetch loop skips its cold refresh for
    /// each hydrated tab).
    ///
    /// The cache carries a per-tab `kind` discriminant naming which
    /// `TabData` variant its `rows` JSON matches. On a name-match
    /// we require the tab's live variant to agree with the cached
    /// kind — a schema drift (a tab whose `kind =` changed between
    /// prefetch and open) silently falls through to a cold fetch
    /// for that tab. Silent fall-through on every possible error
    /// (env unset, file missing, bad JSON, no name match, no kind
    /// match). Losing hydration is never worse than cold-fetch
    /// behavior.
    fn hydrate_from_prefetch_cache(&mut self) -> bool {
        use std::collections::HashSet;

        #[derive(serde::Deserialize)]
        struct PrefetchCache {
            #[serde(default)]
            #[allow(dead_code)]
            generated_at: u64,
            tabs: Vec<PrefetchTab>,
        }
        #[derive(serde::Deserialize)]
        struct PrefetchTab {
            name: String,
            kind: String,
            #[serde(default)]
            rows: serde_json::Value,
        }
        let Ok(path) = std::env::var("MNML_PREFETCH_CACHE_FILE") else {
            return false;
        };
        let Ok(body) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(cache) = serde_json::from_str::<PrefetchCache>(&body) else {
            return false;
        };
        let mut any = false;
        for pt in cache.tabs {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.name == pt.name) else {
                continue;
            };
            // Kind guard: only hydrate when the cached shape matches
            // the tab's live variant. A schema drift between cache
            // and app falls through to a cold refresh — always
            // strictly better than telling the pane "already
            // fetched" with a wrong shape.
            let hydrated = match (pt.kind.as_str(), &mut tab.data) {
                ("PullRequests", TabData::PullRequests(dst)) => {
                    match serde_json::from_value::<Vec<PullRequest>>(pt.rows) {
                        Ok(v) => {
                            *dst = v;
                            true
                        }
                        Err(_) => false,
                    }
                }
                ("Pipelines", TabData::Pipelines(dst)) => {
                    match serde_json::from_value::<Vec<Pipeline>>(pt.rows) {
                        Ok(v) => {
                            *dst = v;
                            true
                        }
                        Err(_) => false,
                    }
                }
                ("Branches", TabData::Branches(dst)) => {
                    match serde_json::from_value::<Vec<BranchRef>>(pt.rows) {
                        Ok(v) => {
                            *dst = v;
                            true
                        }
                        Err(_) => false,
                    }
                }
                ("RepoTree", TabData::RepoTree { rows, expanded }) => {
                    match serde_json::from_value::<Vec<RepoPipelines>>(pt.rows) {
                        Ok(v) => {
                            *rows = v;
                            *expanded = HashSet::new();
                            true
                        }
                        Err(_) => false,
                    }
                }
                (
                    "RepoPrTree",
                    TabData::RepoPrTree {
                        rows,
                        expanded,
                        show_all,
                    },
                ) => match serde_json::from_value::<Vec<RepoPrs>>(pt.rows) {
                    Ok(v) => {
                        *rows = v;
                        *expanded = HashSet::new();
                        *show_all = false;
                        true
                    }
                    Err(_) => false,
                },
                _ => false,
            };
            if hydrated {
                tab.last_fetched = Some(std::time::Instant::now());
                tab.last_error = None;
                any = true;
            }
        }
        if any {
            self.status = format!("hydrated from prefetch cache · {}", path);
        }
        any
    }

    /// #1103 (2026-08-20) — toggle the current tab's `mine_only`
    /// server-side filter and invalidate the tab data so the next
    /// refresh re-fetches with the new predicate. Wired into the
    /// filter toolbar's "All ▾" chip (label swaps between "All" /
    /// "Authored by me"). Only meaningful on WorkspaceOpenPRs /
    /// WorkspaceMergedPRs; a no-op elsewhere so click-through on
    /// other tab kinds doesn't corrupt state.
    pub fn toggle_active_mine_only(&mut self) {
        use crate::app::TabKind;
        let tab = &mut self.tabs[self.active_tab];
        match tab.spec.kind {
            TabKind::WorkspaceOpenPRs | TabKind::WorkspaceMergedPRs => {
                tab.spec.mine_only = !tab.spec.mine_only;
                tab.last_fetched = None;
                tab.last_error = None;
                self.status = format!(
                    "{}: filter → {}",
                    tab.name,
                    if tab.spec.mine_only {
                        "Authored by me"
                    } else {
                        "All"
                    }
                );
            }
            _ => {
                self.status = "Author filter not supported on this tab".into();
            }
        }
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            if self.tabs[idx].last_fetched.is_none() {
                self.status = format!("loading {}…", self.tabs[idx].name);
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.active().data.len();
        if len == 0 {
            return;
        }
        let s = self.active().selected as isize + delta;
        let new = s.clamp(0, len as isize - 1) as usize;
        self.active_mut().selected = new;
    }

    /// `(workspace, repo, id)` of the focused PR, or `None` if the
    /// active tab isn't a PR tab or has no rows. Used as the detail
    /// cache key.
    pub fn focused_key(&self) -> Option<(String, String, i64)> {
        let tab = self.active();
        // Flat PullRequests tab — direct lookup by cursor.
        if let TabData::PullRequests(prs) = &tab.data {
            let pr = prs.get(tab.selected)?;
            let full = pr.repo_short();
            let (workspace, repo) = full.split_once('/').unwrap_or(("", full.as_str()));
            return Some((workspace.to_string(), repo.to_string(), pr.id));
        }
        // #1003 (2026-08-18) — RepoPrTree (workspace_open_prs +
        // workspace_merged_prs) exposes PR rows too, just nested
        // under repo headers. `focused_pr()` unwinds the tree cursor
        // to a specific `(slug, PR)` when the row is a PR leaf
        // (repo headers return None there). Reuse the tab's
        // workspace as the ws component — RepoPrs rows carry only
        // the short slug, and workspace tabs are single-workspace
        // by construction.
        if let Some((slug, pr)) = self.focused_pr() {
            let full = pr.repo_short();
            let (workspace, repo) = full.split_once('/').unwrap_or(("", full.as_str()));
            // Prefer the full `owner/slug` split from the PR's own
            // `repo_short()` (guarantees the workspace-scope tab
            // shows the right owner). Fall back to `(cfg.workspace,
            // slug)` when the split didn't produce a workspace half.
            let workspace = if workspace.is_empty() {
                self.cfg.workspace.as_str()
            } else {
                workspace
            };
            let repo = if repo.is_empty() { slug.as_str() } else { repo };
            return Some((workspace.to_string(), repo.to_string(), pr.id));
        }
        None
    }

    /// Resolve the workspace-wide repo scope. Applies `Config::scope`
    /// (`all` / `recent` / `explicit`), subtracts `hidden_repos`,
    /// and applies `repo_order` for display ordering. Cached in
    /// `self.scope_repos` so three sibling workspace tabs share one
    /// API round-trip; call `invalidate_scope()` after a scope
    /// toggle. tree-redesign 2026-07-14.
    pub async fn resolved_scope_repos(&mut self, workspace: &str) -> Result<Vec<String>> {
        if let Some(cached) = &self.scope_repos {
            return Ok(cached.clone());
        }
        let hidden: HashSet<String> = self.cfg.hidden_repos.iter().cloned().collect();
        // #1031 (2026-08-18) — the integration-level `repos` allowlist
        // (originally scoped to `--values` in #1028) also drives the
        // workspace tabs when set. Bypasses `scope`/`explicit_repos`
        // entirely — if the user has expressed "these are the repos I
        // care about" via `repos`, honor it everywhere and avoid the
        // enumerate-then-filter round-trip. Hidden + repo_order still
        // apply.
        let raw: Vec<String> = if !self.cfg.repos.is_empty() {
            self.cfg.repos.clone()
        } else {
            match self.cfg.scope.as_str() {
                "explicit" => self.cfg.explicit_repos.clone(),
                "recent" => {
                    let activity = self
                        .client
                        .list_workspace_repos_with_activity(workspace)
                        .await?;
                    let cutoff = std::time::SystemTime::now()
                        .checked_sub(std::time::Duration::from_secs(
                            self.cfg.recent_window_days as u64 * 86_400,
                        ))
                        .unwrap_or(std::time::UNIX_EPOCH);
                    let cutoff_secs = cutoff
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    activity
                        .into_iter()
                        .filter(|r| {
                            r.updated_on
                                .as_deref()
                                .and_then(parse_iso_seconds)
                                .is_some_and(|ts| ts >= cutoff_secs)
                        })
                        .map(|r| r.slug)
                        .collect()
                }
                // Default / "all" — every repo, in Bitbucket's
                // -updated_on order (activity DESC).
                _ => self
                    .client
                    .list_workspace_repos_with_activity(workspace)
                    .await?
                    .into_iter()
                    .map(|r| r.slug)
                    .collect(),
            }
        };
        // Subtract hidden.
        let after_hide: Vec<String> = raw.into_iter().filter(|s| !hidden.contains(s)).collect();
        // Apply repo_order: user-listed slugs first (in listed order),
        // then the rest in Bitbucket's returned order.
        let ordered: Vec<String> = if self.cfg.repo_order.is_empty() {
            after_hide
        } else {
            let order_set: HashSet<&String> = self.cfg.repo_order.iter().collect();
            let mut head: Vec<String> = self
                .cfg
                .repo_order
                .iter()
                .filter(|s| after_hide.contains(s))
                .cloned()
                .collect();
            let tail: Vec<String> = after_hide
                .into_iter()
                .filter(|s| !order_set.contains(s))
                .collect();
            head.extend(tail);
            head
        };
        self.scope_repos = Some(ordered.clone());
        Ok(ordered)
    }

    /// Drop the cached scope so the next workspace-wide refresh
    /// re-fetches it. Call after any config mutation that would
    /// change which repos show up (scope toggle, hide/unhide,
    /// reorder). tree-redesign 2026-07-14.
    pub fn invalidate_scope(&mut self) {
        self.scope_repos = None;
    }

    // ── tree-redesign 2026-07-14 phase 2d — repo-tree actions ────

    /// Map the active tab's `selected` visible-row index → the
    /// underlying `(repo_slug, Option<branch_name>)`. `None`
    /// branch = the row is a repo header; `Some(name)` = it's an
    /// indented branch row under the expanded repo above.
    /// Returns None when the active tab isn't a RepoTree or the
    /// index is out of range. Used by every phase-2d action to
    /// resolve "what's under the cursor."
    pub fn tree_focused_row(&self) -> Option<(String, Option<String>)> {
        // Uniform slug/child-label walk that works over both tree
        // variants. `child_label` is the branch name for RepoTree,
        // a "PR #N" synthetic label for RepoPrTree — good enough
        // for expand/collapse/hide/reorder decisions which only
        // care about which repo header the cursor sits under.
        let (rows_iter, expanded): (BoxedTreeRowsIter<'_>, &HashSet<String>) =
            match &self.active().data {
                TabData::RepoTree { rows, expanded } => (
                    Box::new(rows.iter().map(|r| {
                        (
                            r.slug.clone(),
                            r.branches.iter().map(|b| b.name.clone()).collect(),
                        )
                    })),
                    expanded,
                ),
                TabData::RepoPrTree { rows, expanded, .. } => (
                    Box::new(rows.iter().map(|r| {
                        (
                            r.slug.clone(),
                            r.prs.iter().map(|p| format!("PR #{}", p.id)).collect(),
                        )
                    })),
                    expanded,
                ),
                _ => return None,
            };
        let mut idx = self.active().selected;
        for (slug, children) in rows_iter {
            if idx == 0 {
                return Some((slug, None));
            }
            idx -= 1;
            if expanded.contains(&slug) {
                for child in children {
                    if idx == 0 {
                        return Some((slug, Some(child)));
                    }
                    idx -= 1;
                }
            }
        }
        None
    }

    /// Space / Enter on a repo header row → toggle expand/collapse.
    /// No-op on branch rows (no children to expand). Also snaps the
    /// selection to stay on the same repo header after collapse so
    /// the visible-row cursor doesn't jump into an unrelated repo.
    /// Access `expanded` mutably regardless of which tree variant
    /// the active tab holds. Returns None on non-tree tabs.
    fn tree_expanded_mut(&mut self) -> Option<&mut HashSet<String>> {
        match &mut self.active_mut().data {
            TabData::RepoTree { expanded, .. } | TabData::RepoPrTree { expanded, .. } => {
                Some(expanded)
            }
            _ => None,
        }
    }

    /// Slugs of the active tree tab's repos, in display order.
    /// Empty vec for non-tree tabs.
    fn tree_repo_slugs(&self) -> Vec<String> {
        match &self.active().data {
            TabData::RepoTree { rows, .. } => rows.iter().map(|r| r.slug.clone()).collect(),
            TabData::RepoPrTree { rows, .. } => rows.iter().map(|r| r.slug.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Child count for a repo slug — branches on RepoTree, PRs on
    /// RepoPrTree. Used by the cursor-snap-to-parent logic after
    /// a collapse from a child row.
    fn tree_child_count(&self, slug: &str) -> usize {
        match &self.active().data {
            TabData::RepoTree { rows, .. } => rows
                .iter()
                .find(|r| r.slug == slug)
                .map(|r| r.branches.len())
                .unwrap_or(0),
            TabData::RepoPrTree { rows, .. } => rows
                .iter()
                .find(|r| r.slug == slug)
                .map(|r| r.prs.len())
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// 2026-07-24 — walk a RepoPrTree the same way `tree_focused_row`
    /// does, but return the full PR when the cursor is on a PR row.
    /// Returns `Some((slug, pr))` only when the active tab is a
    /// RepoPrTree AND the focus is under an expanded repo AND the
    /// row is a PR (not the repo header). `None` otherwise.
    pub fn focused_pr(&self) -> Option<(String, PullRequest)> {
        let (rows, expanded, show_all) = match &self.active().data {
            TabData::RepoPrTree {
                rows,
                expanded,
                show_all,
            } => (rows, expanded, *show_all),
            _ => return None,
        };
        let mut idx = self.active().selected;
        for repo in rows {
            if idx == 0 {
                return None; // repo header
            }
            idx -= 1;
            if expanded.contains(&repo.slug) {
                // 2026-07-24 — mirror the 24h filter applied at render.
                for pr in repo.prs.iter().filter(|pr| {
                    if show_all {
                        return true;
                    }
                    pr.updated_on
                        .as_deref()
                        .and_then(hours_since)
                        .map(|h| h <= RECENT_WINDOW_HOURS)
                        .unwrap_or(true)
                }) {
                    if idx == 0 {
                        return Some((repo.slug.clone(), pr.clone()));
                    }
                    idx -= 1;
                }
            }
        }
        None
    }

    /// 2026-07-24 — toggle the "expanded" flag on the focused MERGED
    /// PR row. If we're expanding and haven't fetched the
    /// post-merge pipeline yet, kicks off the fetch and populates
    /// `pr_pipeline_cache`. No-op when focus isn't on a merged PR
    /// row with a merge_commit hash.
    pub async fn toggle_pr_expand(&mut self) {
        let Some((slug, pr)) = self.focused_pr() else {
            return;
        };
        if !pr.state.eq_ignore_ascii_case("MERGED") {
            return;
        }
        let Some(hash) = pr.merge_commit.as_ref().map(|c| c.hash.clone()) else {
            return;
        };
        let key = (slug.clone(), pr.id);
        if self.expanded_prs.contains(&key) {
            self.expanded_prs.remove(&key);
            return;
        }
        self.expanded_prs.insert(key.clone());
        if self.pr_pipeline_cache.contains_key(&key) {
            return;
        }
        self.status = format!(
            "fetching pipeline for PR #{} on {}…",
            pr.id,
            short_sha(&hash)
        );
        let workspace = self.cfg.workspace.clone();
        match self
            .client
            .list_pipelines_by_commit(&workspace, &slug, &hash)
            .await
        {
            Ok(pipelines) => {
                let n = pipelines.len();
                self.pr_pipeline_cache.insert(key, pipelines);
                self.status = format!("PR #{}: {n} pipeline(s) on merge commit", pr.id);
            }
            Err(e) => {
                self.pr_pipeline_cache.insert(key, Vec::new());
                self.status = format!("PR #{} pipeline fetch failed: {e}", pr.id);
            }
        }
    }

    /// 2026-07-24 — set the RepoPrTree `show_all` flag on the
    /// active tab. Called when the user activates the "[ Show N
    /// older PRs ]" footer row (click or Enter). No-op on non-
    /// RepoPrTree tabs.
    pub fn set_show_all_prs(&mut self, value: bool) {
        if let TabData::RepoPrTree { show_all, .. } = &mut self.active_mut().data {
            *show_all = value;
        }
    }

    /// 2026-07-24 — Space/Enter dispatch entry point. Routes to
    /// `toggle_pr_expand` when the focus is on a merged PR row;
    /// falls back to `tree_toggle_focused_repo` otherwise. Async
    /// because the PR-expand path fetches the post-merge pipeline
    /// inline on first expand.
    pub async fn smart_toggle_focused(&mut self) {
        // 2026-07-24 — footer row activation. When the cursor is on
        // the synthetic "[ Show N older PRs ]" row (the very last
        // visible row when the recency filter has hidden anything),
        // Enter/Space flips the flag instead of doing repo/PR toggling.
        if self.focus_is_show_more_footer() {
            self.set_show_all_prs(true);
            return;
        }
        if let Some((_, pr)) = self.focused_pr()
            && pr.state.eq_ignore_ascii_case("MERGED")
            && pr.merge_commit.is_some()
        {
            self.toggle_pr_expand().await;
            return;
        }
        self.tree_toggle_focused_repo();
    }

    /// 2026-07-24 — true when the active tab is a RepoPrTree with
    /// the "[ Show N older ]" footer row rendered AND the cursor
    /// sits on that final row. Used by `smart_toggle_focused` to
    /// route Enter/Space onto the flag flip.
    fn focus_is_show_more_footer(&self) -> bool {
        let TabData::RepoPrTree { show_all, .. } = &self.active().data else {
            return false;
        };
        if *show_all {
            return false;
        }
        let hidden = self.active().data.hidden_pr_count().unwrap_or(0);
        if hidden == 0 {
            return false;
        }
        // Footer is always the last visible row: len() - 1 when the
        // footer is rendered. `TabData::len` accounts for it.
        self.active().selected + 1 == self.active().data.len()
    }

    /// 2026-07-24 — Left/h dispatch entry point. On an expanded
    /// merged PR row: collapse. Otherwise: repo-level collapse-or-
    /// ascend.
    pub fn smart_collapse_focused(&mut self) {
        if let Some((slug, pr)) = self.focused_pr() {
            let key = (slug, pr.id);
            if self.expanded_prs.contains(&key) {
                self.expanded_prs.remove(&key);
                return;
            }
        }
        self.tree_collapse_focused();
    }

    /// 2026-07-24 — Right/l dispatch entry point. On a merged PR
    /// row: expand-if-collapsed (never collapses; Left/h does that).
    /// Otherwise: repo-level expand-or-descend.
    pub async fn smart_expand_focused(&mut self) {
        if let Some((slug, pr)) = self.focused_pr()
            && pr.state.eq_ignore_ascii_case("MERGED")
            && pr.merge_commit.is_some()
        {
            let key = (slug, pr.id);
            if !self.expanded_prs.contains(&key) {
                self.toggle_pr_expand().await;
            }
            return;
        }
        self.tree_expand_focused();
    }

    pub fn tree_toggle_focused_repo(&mut self) {
        let Some((slug, child)) = self.tree_focused_row() else {
            return;
        };
        // Toggle expansion (same for both variants).
        let Some(expanded) = self.tree_expanded_mut() else {
            return;
        };
        if !expanded.insert(slug.clone()) {
            expanded.remove(&slug);
        }
        // If we were sitting on a child row, snap the cursor back
        // to the parent-repo header row so the visible-index
        // doesn't fly into an unrelated repo after collapse.
        if child.is_some() {
            let slugs = self.tree_repo_slugs();
            let expanded_snapshot: HashSet<String> = self
                .tree_expanded_mut()
                .map(|e| e.clone())
                .unwrap_or_default();
            let mut idx = 0usize;
            for s in &slugs {
                if s == &slug {
                    self.active_mut().selected = idx;
                    return;
                }
                idx += 1;
                if expanded_snapshot.contains(s) {
                    idx += self.tree_child_count(s);
                }
            }
        }
    }

    /// Right arrow on tree → expand the focused repo (or move
    /// into first child if already expanded). Matches mnml's file
    /// tree `Right` / `l` behavior (`tree.expand_or_descend`).
    /// No-op on child rows (no grandchildren to descend into).
    /// tree-redesign 2026-07-15.
    pub fn tree_expand_focused(&mut self) {
        let Some((slug, child)) = self.tree_focused_row() else {
            return;
        };
        if child.is_some() {
            return; // already on a child; nothing to descend into
        }
        let already_expanded = self.tree_expanded_mut().is_some_and(|e| e.contains(&slug));
        if !already_expanded {
            if let Some(expanded) = self.tree_expanded_mut() {
                expanded.insert(slug);
            }
        } else {
            // Already expanded → descend into first child (move
            // selected cursor down one row).
            let len = self.active().data.len();
            let s = self.active().selected + 1;
            self.active_mut().selected = s.min(len.saturating_sub(1));
        }
    }

    /// Left arrow on tree → collapse the focused repo (or ascend
    /// to parent if on a child row). Matches mnml's file tree
    /// `Left` / `h` behavior (`tree.collapse_or_ascend`).
    /// tree-redesign 2026-07-15.
    pub fn tree_collapse_focused(&mut self) {
        let Some((slug, child)) = self.tree_focused_row() else {
            return;
        };
        // On a child row → ascend to parent (snap cursor + collapse).
        if child.is_some() {
            let Some(expanded) = self.tree_expanded_mut() else {
                return;
            };
            expanded.remove(&slug);
            // Snap cursor to parent repo header row.
            let slugs = self.tree_repo_slugs();
            let expanded_snapshot: HashSet<String> = self
                .tree_expanded_mut()
                .map(|e| e.clone())
                .unwrap_or_default();
            let mut idx = 0usize;
            for s in &slugs {
                if s == &slug {
                    self.active_mut().selected = idx;
                    return;
                }
                idx += 1;
                if expanded_snapshot.contains(s) {
                    idx += self.tree_child_count(s);
                }
            }
            return;
        }
        // On a repo header → collapse it if expanded, else no-op.
        if let Some(expanded) = self.tree_expanded_mut() {
            expanded.remove(&slug);
        }
    }

    /// `e` on tree → expand every repo.
    pub fn tree_expand_all(&mut self) {
        let slugs = self.tree_repo_slugs();
        if let Some(expanded) = self.tree_expanded_mut() {
            for s in slugs {
                expanded.insert(s);
            }
        }
    }

    /// `c` on tree → collapse every repo. Selection clamped so
    /// it doesn't end up past the (now smaller) visible-row count.
    pub fn tree_collapse_all(&mut self) {
        if let Some(expanded) = self.tree_expanded_mut() {
            expanded.clear();
        }
        let len = self.active().data.len();
        self.active_mut().selected = self.active().selected.min(len.saturating_sub(1));
    }

    /// `x` on tree → append focused repo's slug to
    /// `Config::hidden_repos`, persist, invalidate scope, refresh
    /// so the row disappears immediately.
    pub async fn tree_hide_focused_repo(&mut self) -> Result<()> {
        let Some((slug, _)) = self.tree_focused_row() else {
            return Ok(());
        };
        if !self.cfg.hidden_repos.contains(&slug) {
            self.cfg.hidden_repos.push(slug.clone());
            crate::config::save(&self.cfg)?;
        }
        self.invalidate_scope();
        self.refresh_active().await;
        self.status = format!("hid {slug} (H to un-hide all)");
        Ok(())
    }

    /// `H` on tree → clear hidden_repos entirely + persist.
    pub async fn tree_unhide_all(&mut self) -> Result<()> {
        if self.cfg.hidden_repos.is_empty() {
            self.status = "nothing hidden".into();
            return Ok(());
        }
        let n = self.cfg.hidden_repos.len();
        self.cfg.hidden_repos.clear();
        crate::config::save(&self.cfg)?;
        self.invalidate_scope();
        self.refresh_active().await;
        self.status = format!("un-hid {n} repo(s)");
        Ok(())
    }

    /// `s` on tree → cycle scope `all → recent → explicit → all`,
    /// persist, invalidate, refresh. When switching TO `explicit`
    /// and `explicit_repos` is empty, seeds it with the currently-
    /// visible repos so the tab doesn't blank on the transition.
    pub async fn tree_cycle_scope(&mut self) -> Result<()> {
        let next = match self.cfg.scope.as_str() {
            "all" => "recent",
            "recent" => "explicit",
            _ => "all",
        };
        // Seed explicit_repos from the current visible set on the
        // all→recent→explicit transition so the tab doesn't go
        // blank on the switch. User can later hand-edit the config
        // to trim.
        if next == "explicit"
            && self.cfg.explicit_repos.is_empty()
            && let Some(cached) = &self.scope_repos
        {
            self.cfg.explicit_repos = cached.clone();
        }
        self.cfg.scope = next.to_string();
        crate::config::save(&self.cfg)?;
        self.invalidate_scope();
        self.refresh_active().await;
        self.status = format!("scope: {next}");
        Ok(())
    }

    /// Alt-↑ / Alt-↓ on a repo header row → move it up/down in
    /// `Config::repo_order` (+1 = down, -1 = up). Persists +
    /// re-invalidates scope so the display refreshes. No-op on
    /// branch rows or when the repo is already at the boundary.
    pub fn tree_reorder_focused(&mut self, delta: i32) -> Result<()> {
        let Some((slug, branch)) = self.tree_focused_row() else {
            return Ok(());
        };
        if branch.is_some() {
            return Ok(()); // branches aren't reorderable
        }
        // Take the current visible slugs (either from repo_order or
        // fall back to the resolved scope). Populate repo_order if
        // it's empty so the reorder has a base to modify.
        if self.cfg.repo_order.is_empty()
            && let Some(cached) = &self.scope_repos
        {
            self.cfg.repo_order = cached.clone();
        }
        let pos = self.cfg.repo_order.iter().position(|s| s == &slug);
        let Some(pos) = pos else {
            // Repo isn't in repo_order yet (unusual — implies
            // scope_repos wasn't cached at all). Append + no-op the
            // move, next reorder will land somewhere.
            self.cfg.repo_order.push(slug);
            crate::config::save(&self.cfg)?;
            return Ok(());
        };
        let new_pos = (pos as i32 + delta).clamp(0, self.cfg.repo_order.len() as i32 - 1) as usize;
        if new_pos == pos {
            return Ok(());
        }
        let slug = self.cfg.repo_order.remove(pos);
        self.cfg.repo_order.insert(new_pos, slug);
        crate::config::save(&self.cfg)?;
        self.invalidate_scope();
        // Also update the tree's visible row order in-place so the
        // user sees the move before the next refresh completes.
        // Read the target slug out of cfg BEFORE re-borrowing self
        // mutably for the tab data.
        let target_slug = self.cfg.repo_order[new_pos].clone();
        match &mut self.active_mut().data {
            TabData::RepoTree { rows, .. } => {
                if let Some(p) = rows.iter().position(|r| r.slug == target_slug) {
                    let repo = rows.remove(p);
                    rows.insert(new_pos.min(rows.len()), repo);
                }
            }
            TabData::RepoPrTree { rows, .. } => {
                if let Some(p) = rows.iter().position(|r| r.slug == target_slug) {
                    let repo = rows.remove(p);
                    rows.insert(new_pos.min(rows.len()), repo);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn refresh_active(&mut self) {
        let idx = self.active_tab;
        // Bail out for pre-failed tabs (resolution error in App::new).
        if self.tabs[idx].last_error.is_some() && self.tabs[idx].data.is_empty() {
            self.status = format!(
                "{}: {}",
                self.tabs[idx].name,
                self.tabs[idx].last_error.as_deref().unwrap_or("")
            );
            return;
        }
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("refreshing {name}…");
        match spec.kind {
            TabKind::PullRequests => {
                let result = match (&spec.repo, &spec.scope) {
                    // Per-repo PR list — single endpoint call.
                    (Some(repo), _) => {
                        self.client
                            .list_repo_prs(
                                &spec.workspace,
                                repo,
                                Some(&spec.state),
                                spec.q.as_deref(),
                                50,
                            )
                            .await
                    }
                    // mode = "mine" / "reviewing" — Bitbucket Cloud
                    // has no workspace-scoped PR endpoint. Enumerate
                    // the workspace's repos, fan out per-repo BBQL
                    // author/reviewer queries, merge.
                    (None, Some(WorkspaceScope::UserAuthored { account_id })) => {
                        let bbql = format!("author.account_id = \"{account_id}\"");
                        self.client
                            .list_workspace_prs_filtered(
                                &spec.workspace,
                                &bbql,
                                Some(&spec.state),
                                50,
                            )
                            .await
                    }
                    (None, Some(WorkspaceScope::UserReviewing)) => {
                        let account_id = self.me_account_id.clone().unwrap_or_default();
                        let bbql = format!("reviewers.account_id = \"{account_id}\"");
                        self.client
                            .list_workspace_prs_filtered(
                                &spec.workspace,
                                &bbql,
                                Some(&spec.state),
                                50,
                            )
                            .await
                    }
                    (None, None) => Err(anyhow::anyhow!(
                        "internal: tab has no repo and no workspace scope"
                    )),
                };
                self.commit_pr_refresh(idx, name, result);
            }
            TabKind::Pipelines => {
                let repo = spec.repo.as_deref().unwrap_or("");
                let result = self.client.list_pipelines(&spec.workspace, repo, 50).await;
                self.commit_pipeline_refresh(idx, name, result);
            }
            TabKind::Branches => {
                let repo = spec.repo.as_deref().unwrap_or("");
                let result = self.client.list_branches(&spec.workspace, repo, 50).await;
                self.commit_branch_refresh(idx, name, result);
            }
            // tree-redesign 2026-07-14 phase 2b — workspace-wide
            // dispatch. All three kinds share the same
            // resolved_scope_repos call (cached on App) so entering
            // three sibling tabs in a row makes one repo-enum API
            // round-trip, not three.
            TabKind::WorkspaceOpenPRs => {
                let workspace = spec.workspace.clone();
                let repos = match self.resolved_scope_repos(&workspace).await {
                    Ok(r) => r,
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("scope-resolve error: {e}");
                        return;
                    }
                };
                // #1099 f/u v2 (2026-08-20) — when `mine_only`, filter
                // SERVER-SIDE via BBQL `author.account_id = <me>` per
                // repo. The prior post-fetch client-side filter over
                // the top-25 per repo would silently drop mine PRs
                // older than the 25 most-recently-updated PRs in a
                // busy repo — the exact reason the chip counter said
                // "4" but the pane rendered "0 rows". Now the pane
                // fetches ONLY the user's own PRs per repo (no cap
                // pressure), then groups into RepoPrs.
                let mut result = if spec.mine_only
                    && let Some(me) = self.me_account_id.as_deref()
                {
                    // #1099 f/u v3 (2026-08-21) — state clause must be
                    // baked INTO the BBQL predicate. Bitbucket ignores
                    // the URL `?state=` param when `q=` is also set,
                    // so the earlier passthrough returned every state
                    // (500+ historical merges per repo).
                    //
                    // Predicate: `(state = "OPEN" OR state = "MERGED")
                    // AND author.account_id = X`. That gives every
                    // open PR (what the user cares about now) plus
                    // enough recent merged to render "1 peek + Show
                    // N more merged" via `visible_prs_for_render`.
                    // Page size 20 is a firm cap — enough to cover
                    // most people's open + a handful of recent
                    // merges, tiny enough that the pane isn't slow
                    // on repos where the user has 500 historical
                    // PRs. The client-side helper then culls to the
                    // display policy.
                    self.client
                        .list_workspace_open_prs_by_repo_bbql(
                            &workspace,
                            &repos,
                            &format!(
                                "(state = \"OPEN\" OR state = \"MERGED\") \
                                 AND author.account_id = \"{me}\""
                            ),
                            20,
                        )
                        .await
                } else {
                    // tree-redesign 2026-07-15 — user asked for per-repo
                    // drill-down on Open+Draft too. Fetch by-repo so the
                    // tab renders as `TabData::RepoPrTree` (grouped) instead
                    // of a flat PR list.
                    self.client
                        .list_workspace_open_prs_by_repo(&workspace, &repos, 25)
                        .await
                };
                // Post-fetch tidy — drop empty repo headers after
                // filtering (whether server-side or fallback).
                if spec.mine_only
                    && let Ok(ref mut repo_prs) = result
                {
                    repo_prs.retain(|r| !r.prs.is_empty());
                }
                self.commit_pr_tree_refresh(idx, name, result);
            }
            TabKind::WorkspaceMergedPRs => {
                let workspace = spec.workspace.clone();
                let repos = match self.resolved_scope_repos(&workspace).await {
                    Ok(r) => r,
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("scope-resolve error: {e}");
                        return;
                    }
                };
                // Same per-repo drill-down on Merged. Uses a fresh
                // fetch helper that returns Vec<RepoPrs> for state=MERGED.
                let result = self
                    .client
                    .list_workspace_merged_prs_by_repo(&workspace, &repos, 25)
                    .await;
                self.commit_pr_tree_refresh(idx, name, result);
            }
            TabKind::WorkspacePipelines => {
                let workspace = spec.workspace.clone();
                let repos = match self.resolved_scope_repos(&workspace).await {
                    Ok(r) => r,
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("scope-resolve error: {e}");
                        return;
                    }
                };
                let result = self
                    .client
                    // 2026-07-19 — bumped branches_per_repo 25 → 100
                    // so major branches (main / master / develop /
                    // etc.) still land in the fetched set on very
                    // active repos where feature-branch churn would
                    // push them out of the top-25 by commit date.
                    // `curate_branches` in this file trims the display
                    // down to (majors + top-1 feature) after fetch, so
                    // this bumped-per-page count only affects the
                    // reachability of majors, not what the user sees.
                    .list_workspace_pipelines_tree(&workspace, &repos, 100, 100)
                    .await;
                self.commit_tree_refresh(idx, name, result);
            }
        }
    }

    // ── curate_branches (module-private helper) ─────────────────
    // Kept as an impl-less free-standing fn so it can be unit-tested
    // without an App instance.

    fn commit_tree_refresh(
        &mut self,
        idx: usize,
        name: String,
        result: Result<Vec<RepoPipelines>>,
    ) {
        match result {
            Ok(mut rows) => {
                let n = rows.len();
                // 2026-07-19 — curate each repo's branch list to
                // "major branches + one feature branch", matching
                // the mnml-aws-amplify shape (user report: "i want
                // it to work like amplify where we show each major
                // branch and a feature branch too"). Majors sort
                // in a canonical order (main/master → develop →
                // staging → prod → beta/alpha); the top feature is
                // whichever non-major branch has the most-recent
                // pipeline.
                for r in &mut rows {
                    r.branches = curate_branches(std::mem::take(&mut r.branches));
                }
                // Sort repos by their most-recent pipeline created_on.
                rows.sort_by(|a, b| {
                    let ka = a
                        .branches
                        .iter()
                        .filter_map(|br| br.latest_pipeline.as_ref())
                        .filter_map(|p| p.created_on.clone())
                        .max();
                    let kb = b
                        .branches
                        .iter()
                        .filter_map(|br| br.latest_pipeline.as_ref())
                        .filter_map(|p| p.created_on.clone())
                        .max();
                    kb.cmp(&ka)
                });
                // Preserve `expanded` across refreshes so a user's
                // "collapse a few repos" state doesn't get blown away
                // by the auto-refresh timer. Filter to slugs that
                // still exist in the new row set. On the very first
                // fetch (or if the prior state was empty), auto-
                // expand every repo — user asked for this on
                // 2026-07-19 for the split "Bitbucket - Pipelines"
                // chip so pipelines are visible without needing to
                // click each repo open.
                let prior: HashSet<String> = match &self.tabs[idx].data {
                    TabData::RepoTree { expanded, .. } => expanded
                        .iter()
                        .filter(|s| rows.iter().any(|r| &r.slug == *s))
                        .cloned()
                        .collect(),
                    _ => HashSet::new(),
                };
                let carry: HashSet<String> = if prior.is_empty() {
                    rows.iter().map(|r| r.slug.clone()).collect()
                } else {
                    prior
                };
                self.tabs[idx].data = TabData::RepoTree {
                    rows,
                    expanded: carry,
                };
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                let vis = self.tabs[idx].data.len();
                self.tabs[idx].selected = self.tabs[idx].selected.min(vis.saturating_sub(1));
                self.status = format!("{name} · {n} repos");
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// tree-redesign 2026-07-15 — commit a per-repo PR fetch into
    /// `TabData::RepoPrTree`. Mirrors `commit_tree_refresh` but for
    /// PRs. Preserves `expanded` across refreshes (filtered to
    /// slugs still present in the new row set) so the auto-refresh
    /// timer doesn't blow away the user's drill-down state.
    fn commit_pr_tree_refresh(&mut self, idx: usize, name: String, result: Result<Vec<RepoPrs>>) {
        match result {
            Ok(rows) => {
                let n = rows.len();
                let total_prs: usize = rows.iter().map(|r| r.prs.len()).sum();
                // 2026-08-16 (#948) — surface partial-data. Erroring
                // repos are still visible (their row shows "429 ·
                // retry in 30s" or "auth failed") but the status
                // line needs to name the count so the user knows
                // some data is missing rather than the tab being
                // authoritative-empty.
                let errored: usize = rows.iter().filter(|r| r.error.is_some()).count();
                // 2026-07-19 — same auto-expand behavior as the
                // pipelines tab: on the very first fetch (prior
                // expanded set was empty) auto-expand every repo
                // so PRs are visible without hunting-and-clicking.
                // User's post-first-fetch collapse/expand choices
                // are preserved across auto-refreshes.
                let prior: HashSet<String> = match &self.tabs[idx].data {
                    TabData::RepoPrTree { expanded, .. } => expanded
                        .iter()
                        .filter(|s| rows.iter().any(|r| &r.slug == *s))
                        .cloned()
                        .collect(),
                    _ => HashSet::new(),
                };
                let carry: HashSet<String> = if prior.is_empty() {
                    rows.iter().map(|r| r.slug.clone()).collect()
                } else {
                    prior
                };
                self.tabs[idx].data = TabData::RepoPrTree {
                    rows,
                    expanded: carry,
                    // 2026-07-24 — always reset the recency filter
                    // on refresh: what "last 24 hours" means changes
                    // every second, and users typically hit refresh
                    // to see fresh activity, not to keep an old
                    // expansion state.
                    show_all: false,
                };
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                let vis = self.tabs[idx].data.len();
                self.tabs[idx].selected = self.tabs[idx].selected.min(vis.saturating_sub(1));
                self.status = if errored > 0 {
                    format!("{name} · {n} repos, {total_prs} PRs ({errored} errored)")
                } else {
                    format!("{name} · {n} repos, {total_prs} PRs")
                };
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    fn commit_pr_refresh(&mut self, idx: usize, name: String, result: Result<Vec<PullRequest>>) {
        match result {
            Ok(prs) => {
                let n = prs.len();
                self.tabs[idx].data = TabData::PullRequests(prs);
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                self.status = format!("{name} · {n} PRs");
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    fn commit_pipeline_refresh(&mut self, idx: usize, name: String, result: Result<Vec<Pipeline>>) {
        match result {
            Ok(ps) => {
                let n = ps.len();
                self.tabs[idx].data = TabData::Pipelines(ps);
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                self.status = format!("{name} · {n} pipelines");
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    fn commit_branch_refresh(&mut self, idx: usize, name: String, result: Result<Vec<BranchRef>>) {
        match result {
            Ok(bs) => {
                let n = bs.len();
                self.tabs[idx].data = TabData::Branches(bs);
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                self.status = format!("{name} · {n} branches");
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Open whatever the focused row points at in the browser.
    /// Per-kind URL strategy:
    ///   - PR: `pr.html_url()` (Bitbucket sends one in `links.html`)
    ///   - Pipeline: bitbucket.org/<ws>/<repo>/pipelines/results/<n>
    ///   - Branch: bitbucket.org/<ws>/<repo>/branch/<name>
    pub fn open_focused(&mut self) {
        let Some(url) = self.focused_url() else {
            self.status = "no URL for this row".to_string();
            return;
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` on a focused row — copy the same URL `Enter`/`o` would
    /// open into the OS clipboard via `pbcopy` / `xclip` / `wl-copy`
    /// / `clip.exe`. Restores the pre-split mnml palette command
    /// `bitbucket.copy_selected_url` (and `_pr_url`) that disappeared
    /// when the BB panes moved to this sibling.
    pub fn yank_focused_url(&mut self) {
        let Some(url) = self.focused_url() else {
            self.status = "no URL for this row".to_string();
            return;
        };
        match crate::clipboard::copy(&url) {
            Ok(()) => self.status = format!("copied {url}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// Pure helper — produces the URL `Enter`/`o`/`y` would act on
    /// for the focused row. None when the tab is empty or the row
    /// kind has no canonical URL.
    fn focused_url(&self) -> Option<String> {
        let tab = self.active();
        let workspace = tab.spec.workspace.clone();
        let repo = tab.spec.repo.clone().unwrap_or_default();
        match &tab.data {
            TabData::PullRequests(prs) => prs.get(tab.selected).and_then(|p| p.html_url()),
            TabData::Pipelines(ps) => ps.get(tab.selected).map(|p| {
                format!(
                    "https://bitbucket.org/{workspace}/{repo}/pipelines/results/{}",
                    p.build_number
                )
            }),
            TabData::Branches(bs) => bs
                .get(tab.selected)
                .map(|b| format!("https://bitbucket.org/{workspace}/{repo}/branch/{}", b.name)),
            // tree-redesign 2026-07-20 phase 2d — RepoTree row→URL.
            // Uses `tree_focused_row()` (slug + optional child
            // label) to route the URL:
            //   repo header  → /branches (or /pull-requests/)
            //   branch child → /branch/<name>
            //   PR child     → the PR's html_url via lookup
            TabData::RepoTree { .. } => {
                let (slug, child) = self.tree_focused_row()?;
                let workspace = &self.cfg.workspace;
                match child {
                    Some(name) => Some(format!(
                        "https://bitbucket.org/{workspace}/{slug}/branch/{name}"
                    )),
                    None => Some(format!("https://bitbucket.org/{workspace}/{slug}/branches")),
                }
            }
            TabData::RepoPrTree { rows, .. } => {
                let (slug, child) = self.tree_focused_row()?;
                let workspace = &self.cfg.workspace;
                match child {
                    Some(label) => {
                        // child label shape: "PR #<id>"
                        let id: Option<i64> =
                            label.strip_prefix("PR #").and_then(|s| s.parse().ok());
                        id.and_then(|id| {
                            rows.iter()
                                .find(|r| r.slug == slug)
                                .and_then(|r| r.prs.iter().find(|p| p.id == id))
                                .and_then(|p| p.html_url())
                        })
                        .or_else(|| {
                            Some(format!(
                                "https://bitbucket.org/{workspace}/{slug}/pull-requests/"
                            ))
                        })
                    }
                    // 2026-08-16 (#948) — on an empty-with-fallback
                    // header row, hop straight to the fallback merged
                    // PR (Enter/o/y is more useful than a repo-level
                    // /pull-requests/ landing). Falls back to the
                    // repo listing when there's no fallback.
                    None => rows
                        .iter()
                        .find(|r| r.slug == slug)
                        .filter(|r| r.prs.is_empty())
                        .and_then(|r| r.fallback_merged.as_ref())
                        .and_then(|p| p.html_url())
                        .or_else(|| {
                            Some(format!(
                                "https://bitbucket.org/{workspace}/{slug}/pull-requests/"
                            ))
                        }),
                }
            }
        }
    }

    /// Toggle the right-half detail panel. Opening lazily fetches
    /// the detail; closing keeps the cache around. #1003
    /// (2026-08-18) — was PR-only with a `"detail panel is PR-only
    /// in v0.3"` stub toast; now supports RepoPrTree tabs too
    /// (`workspace_open_prs` / `workspace_merged_prs`) via the
    /// extended `focused_key()`. Silently no-ops on tabs with no
    /// PR concept (Pipelines / Branches / RepoTree pipelines) —
    /// `ensure_focused_detail` gates on `focused_key()` returning
    /// Some, so a repo-header cursor or pipeline row just leaves
    /// the panel empty instead of showing a misleading toast.
    pub async fn toggle_details(&mut self) {
        self.details_visible = !self.details_visible;
        self.details_scroll = 0;
        if self.details_visible {
            self.ensure_focused_detail().await;
        }
    }

    pub async fn ensure_focused_detail(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        if self.detail_cache.contains_key(&key) || self.detail_in_flight.as_ref() == Some(&key) {
            return;
        }
        self.detail_in_flight = Some(key.clone());
        let (ws, repo, id) = key.clone();
        let pr_res = self.client.get_pr_detail(&ws, &repo, id).await;
        let comments_res = self.client.get_pr_comments(&ws, &repo, id).await;
        self.detail_in_flight = None;
        match (pr_res, comments_res) {
            (Ok(pr), Ok(comments)) => {
                self.detail_cache.insert(key, DetailEntry { pr, comments });
            }
            (Err(e), _) | (_, Err(e)) => {
                self.status = format!("detail fetch failed: {e}");
            }
        }
    }

    pub fn invalidate_focused_detail(&mut self) {
        if let Some(key) = self.focused_key() {
            self.detail_cache.remove(&key);
        }
    }

    pub async fn toggle_approval(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        let Some(me) = self.me_account_id.clone() else {
            self.status = "approve needs Account:Read on the app password".to_string();
            return;
        };
        let approved = self
            .detail_cache
            .get(&key)
            .map(|d| d.pr.approved_by(&me))
            .unwrap_or(false);
        let (ws, repo, id) = key.clone();
        let result = if approved {
            self.client.unapprove_pr(&ws, &repo, id).await
        } else {
            self.client.approve_pr(&ws, &repo, id).await
        };
        match result {
            Ok(()) => {
                self.status = if approved {
                    format!("unapproved {ws}/{repo}#{id}")
                } else {
                    format!("approved {ws}/{repo}#{id}")
                };
                self.detail_cache.remove(&key);
                self.ensure_focused_detail().await;
            }
            Err(e) => {
                self.status = format!("approval toggle failed: {e}");
            }
        }
    }

    pub fn scroll_detail(&mut self, delta: i32) {
        if !self.details_visible {
            return;
        }
        if delta >= 0 {
            self.details_scroll = self.details_scroll.saturating_add(delta as u16);
        } else {
            self.details_scroll = self.details_scroll.saturating_sub((-delta) as u16);
        }
    }
}

/// Parse a Bitbucket ISO-8601 timestamp (e.g. `2026-06-27T21:59:39.415826+00:00`)
/// to Unix seconds. Best-effort — returns `None` on any parse failure so a
/// single malformed timestamp doesn't blank a whole tab. Uses `chrono` when
/// available; falls back to a manual scan of the leading `YYYY-MM-DDTHH:MM:SS`
/// (ignoring fractional + timezone). tree-redesign 2026-07-14.
fn short_sha(hash: &str) -> String {
    hash.chars().take(7).collect()
}

/// 2026-07-24 — hours between an ISO-8601 timestamp and now (UTC).
/// Negative = future. `None` when the string doesn't parse — callers
/// treat that as "unknown age, keep it" so parse failures don't
/// silently hide rows.
pub fn hours_since(iso: &str) -> Option<i64> {
    let then = parse_iso_seconds(iso)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((now - then) / 3600)
}

/// 2026-07-24 — recency window for the "last 24 hours" default view.
/// Constant here so keybindings / render / status text all agree.
pub const RECENT_WINDOW_HOURS: i64 = 24;

/// 2026-07-24 — for a repo's PR list, return `(visible_count,
/// hidden_count)`. When `show_all` is true, everything is visible.
/// Otherwise anything updated more than RECENT_WINDOW_HOURS ago is
/// counted as hidden. Timestamps that fail to parse fall into the
/// visible bucket (better to show than to silently drop).
pub fn count_recent_prs(prs: &[PullRequest], show_all: bool) -> (usize, usize) {
    if show_all {
        return (prs.len(), 0);
    }
    let mut vis = 0;
    let mut hid = 0;
    for pr in prs {
        let recent = pr
            .updated_on
            .as_deref()
            .and_then(hours_since)
            .map(|h| h <= RECENT_WINDOW_HOURS)
            .unwrap_or(true);
        if recent {
            vis += 1;
        } else {
            hid += 1;
        }
    }
    (vis, hid)
}

pub(crate) fn parse_iso_seconds(s: &str) -> Option<i64> {
    // Manual scan: split on `T`, then parse both halves with a fixed shape.
    let (date, rest) = s.split_once('T')?;
    let mut date_parts = date.splitn(3, '-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    // Truncate rest at `.` or `+` or `-` or `Z` — we only want HH:MM:SS.
    let time_end = rest.find(['.', '+', '-', 'Z']).unwrap_or(rest.len());
    let time = &rest[..time_end];
    let mut time_parts = time.splitn(3, ':');
    let hh: i64 = time_parts.next()?.parse().ok()?;
    let mm: i64 = time_parts.next()?.parse().ok()?;
    let ss: i64 = time_parts.next()?.parse().ok()?;
    // Howard Hinnant's civil_from_days algorithm, inverted.
    // Reference: http://howardhinnant.github.io/date_algorithms.html
    // Returns days since 1970-01-01 (Unix epoch, in UTC).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let m = month as u64;
    let d = day as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe as i64 - 719_468; // days since 1970-01-01
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn parse_iso_seconds_matches_known_timestamps() {
        // Verified via `date -u -j -f '%Y-%m-%dT%H:%M:%SZ' '<ts>' +%s` on macOS.
        assert_eq!(
            super::parse_iso_seconds("2026-07-15T13:30:00Z"),
            Some(1_784_122_200)
        );
        assert_eq!(super::parse_iso_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            super::parse_iso_seconds("2000-01-01T00:00:00Z"),
            Some(946_684_800)
        );
        // Fractional + `+00:00` timezone survives the truncation
        // (Bitbucket's actual response format).
        assert_eq!(
            super::parse_iso_seconds("2026-06-27T21:59:39.415826+00:00"),
            Some(1_782_597_579)
        );
    }

    fn t(name: &str) -> Tab {
        Tab {
            name: name.into(),
            kind: "pull_requests".into(),
            workspace: None,
            repo: None,
            state: "OPEN".into(),
            mode: None,
            q: None,
            mine_only: false,
        }
    }

    #[test]
    fn resolve_repo_tab_uses_default_workspace() {
        let mut tab = t("repo");
        tab.repo = Some("example-api".into());
        let spec = TabSpec::resolve(&tab, "acme", None).unwrap();
        assert_eq!(spec.kind, TabKind::PullRequests);
        assert_eq!(spec.workspace, "acme");
        assert_eq!(spec.repo.as_deref(), Some("example-api"));
        assert_eq!(spec.state, "OPEN");
        assert!(spec.q.is_none());
    }

    #[test]
    fn resolve_tab_workspace_overrides_default() {
        let mut tab = t("repo");
        tab.workspace = Some("otherws".into());
        tab.repo = Some("repoA".into());
        let spec = TabSpec::resolve(&tab, "default", None).unwrap();
        assert_eq!(spec.workspace, "otherws");
    }

    #[test]
    fn resolve_mine_sets_user_authored_scope() {
        // 2026-07-09 — `mode = "mine"` now routes to the workspace-
        // wide `/2.0/pullrequests/{account_id}` endpoint via the
        // `WorkspaceScope::UserAuthored` variant. The BBQL author
        // filter is no longer needed (the URL itself scopes to the
        // user).
        let mut tab = t("mine");
        tab.mode = Some("mine".into());
        let spec = TabSpec::resolve(&tab, "ws", Some("aid:abc")).unwrap();
        assert!(spec.repo.is_none());
        assert_eq!(
            spec.scope,
            Some(WorkspaceScope::UserAuthored {
                account_id: "aid:abc".to_string(),
            })
        );
    }

    #[test]
    fn resolve_reviewing_sets_user_reviewing_scope() {
        // Reviewing has no workspace-wide endpoint; resolution still
        // succeeds (so tab titles render) but the fetch surfaces a
        // "not supported" error at refresh time.
        let mut tab = t("rev");
        tab.mode = Some("reviewing".into());
        let spec = TabSpec::resolve(&tab, "ws", Some("aid:abc")).unwrap();
        assert!(spec.repo.is_none());
        assert_eq!(spec.scope, Some(WorkspaceScope::UserReviewing));
    }

    #[test]
    fn resolve_mine_without_account_id_errors() {
        let mut tab = t("mine");
        tab.mode = Some("mine".into());
        let err = TabSpec::resolve(&tab, "ws", None).unwrap_err();
        assert!(err.to_string().contains("Account:Read"));
    }

    #[test]
    fn resolve_mine_preserves_user_supplied_q() {
        // Custom BBQL still flows through — layered by the client
        // (which the user-scoped endpoint doesn't accept in v1, but
        // the field is preserved for future support / per-repo
        // fallback).
        let mut tab = t("mine");
        tab.mode = Some("mine".into());
        tab.q = Some("state != \"DECLINED\"".into());
        let spec = TabSpec::resolve(&tab, "ws", Some("aid:abc")).unwrap();
        assert_eq!(spec.q.as_deref(), Some("state != \"DECLINED\""));
    }

    #[test]
    fn resolve_pipelines_kind_requires_repo() {
        let mut tab = t("p");
        tab.kind = "pipelines".into();
        let err = TabSpec::resolve(&tab, "ws", None).unwrap_err();
        assert!(err.to_string().contains("repo"));
    }

    #[test]
    fn resolve_pipelines_kind_with_repo_succeeds() {
        let mut tab = t("p");
        tab.kind = "pipelines".into();
        tab.repo = Some("myrepo".into());
        let spec = TabSpec::resolve(&tab, "ws", None).unwrap();
        assert_eq!(spec.kind, TabKind::Pipelines);
        assert_eq!(spec.repo.as_deref(), Some("myrepo"));
    }

    #[test]
    fn resolve_branches_kind_requires_repo() {
        let mut tab = t("b");
        tab.kind = "branches".into();
        let err = TabSpec::resolve(&tab, "ws", None).unwrap_err();
        assert!(err.to_string().contains("repo"));
    }

    #[test]
    fn resolve_unknown_kind_errors() {
        let mut tab = t("bad");
        tab.kind = "garbage".into();
        let err = TabSpec::resolve(&tab, "ws", None).unwrap_err();
        assert!(err.to_string().contains("garbage"));
    }
}
