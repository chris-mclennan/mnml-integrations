//! App state — tab list, loaded data, tree navigation.
//!
//! Five tab kinds:
//!   - `Issues`  — legacy search API tab (issues + PRs via `is:pr`).
//!   - `Actions` — legacy per-repo Actions run list.
//!   - `WorkspaceOpenPrs`    — owner-wide OPEN PRs, grouped by repo.
//!   - `WorkspaceMergedPrs`  — owner-wide MERGED PRs, grouped by repo.
//!   - `WorkspaceActions`    — owner-wide recent Actions, grouped by repo.
//!
//! workspace-tabs 2026-08-22 (task #1092) — mirrors the design that
//! landed in mnml-forge-bitbucket 0.3.29.

use crate::config::{Config, Tab};
use crate::github::{Client, Issue, PullRequest, RepoActions, RepoPrs, WorkflowRun};
use anyhow::Result;
use std::collections::HashSet;

/// Recency window for the "last 24 hours" default filter on the
/// workspace_open_prs / workspace_merged_prs tabs. Older PRs collapse
/// under a "[ Show N older ]" footer so the tree stays quiet by
/// default. Matches bitbucket sibling's 24h default.
pub const RECENT_WINDOW_HOURS: i64 = 24;

pub struct App {
    pub cfg: Config,
    pub client: Client,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// Cached `owner → resolved repo slugs`, filled once per owner
    /// per session (invalidated by `s`/`x`/`H`/`Alt-arrow`). Shared
    /// across sibling workspace_* tabs so three tabs cost one repo
    /// enumeration.
    pub scope_repos: Option<Vec<String>>,
    /// Auth user's login, resolved at startup. Drives the `mine_only`
    /// filter on workspace PR tabs.
    pub me_login: Option<String>,
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: TabData,
    pub selected: usize,
    pub last_fetched: Option<std::time::Instant>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    // Legacy per-repo.
    Issues,
    Actions,
    // Workspace-wide.
    WorkspaceOpenPrs,
    WorkspaceMergedPrs,
    WorkspaceActions,
}

impl TabKind {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "issues" => Ok(Self::Issues),
            "actions" => Ok(Self::Actions),
            "workspace_open_prs" => Ok(Self::WorkspaceOpenPrs),
            "workspace_merged_prs" => Ok(Self::WorkspaceMergedPrs),
            "workspace_actions" => Ok(Self::WorkspaceActions),
            other => Err(anyhow::anyhow!("unknown tab kind: {other}")),
        }
    }

    #[allow(dead_code)]
    pub fn is_workspace_wide(self) -> bool {
        matches!(
            self,
            Self::WorkspaceOpenPrs | Self::WorkspaceMergedPrs | Self::WorkspaceActions
        )
    }
}

/// Resolved tab fetch spec. Captured at App::new so refresh doesn't
/// have to re-resolve every call.
#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: TabKind,
    pub owner: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub query: Option<String>,
    pub mine_only: bool,
}

impl TabSpec {
    pub fn resolve(tab: &Tab, default_owner: &str) -> Result<Self> {
        let kind = TabKind::from_str(&tab.kind)?;
        let owner = tab
            .owner
            .clone()
            .unwrap_or_else(|| default_owner.to_string());
        match kind {
            TabKind::Issues => Ok(TabSpec {
                kind,
                owner,
                repo: None,
                branch: None,
                query: tab.query.clone(),
                mine_only: false,
            }),
            TabKind::Actions => {
                let repo = tab
                    .repo
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("kind = `actions` requires `repo`"))?;
                Ok(TabSpec {
                    kind,
                    owner,
                    repo: Some(repo),
                    branch: tab.branch.clone(),
                    query: None,
                    mine_only: false,
                })
            }
            TabKind::WorkspaceOpenPrs | TabKind::WorkspaceMergedPrs | TabKind::WorkspaceActions => {
                Ok(TabSpec {
                    kind,
                    owner,
                    repo: None,
                    branch: None,
                    query: None,
                    mine_only: tab.mine_only,
                })
            }
        }
    }
}

/// Loaded data — variant chosen at tab-spec resolve time.
#[derive(Debug, Clone)]
pub enum TabData {
    Issues(Vec<Issue>),
    Actions(Vec<WorkflowRun>),
    /// Per-repo PR grouping used by workspace_open_prs +
    /// workspace_merged_prs.
    RepoPrTree {
        rows: Vec<RepoPrs>,
        expanded: HashSet<String>,
        /// When false, PRs older than `RECENT_WINDOW_HOURS` collapse
        /// under a "[ Show N older ]" footer. Reset on every refresh
        /// so the recency filter re-applies against fresh fetches.
        show_all: bool,
    },
    /// Per-repo Actions-run grouping used by workspace_actions.
    RepoActionsTree {
        rows: Vec<RepoActions>,
        expanded: HashSet<String>,
    },
}

impl TabData {
    pub fn empty_for(kind: TabKind) -> Self {
        match kind {
            TabKind::Issues => Self::Issues(Vec::new()),
            TabKind::Actions => Self::Actions(Vec::new()),
            TabKind::WorkspaceOpenPrs | TabKind::WorkspaceMergedPrs => Self::RepoPrTree {
                rows: Vec::new(),
                expanded: HashSet::new(),
                show_all: false,
            },
            TabKind::WorkspaceActions => Self::RepoActionsTree {
                rows: Vec::new(),
                expanded: HashSet::new(),
            },
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Issues(v) => v.len(),
            Self::Actions(v) => v.len(),
            Self::RepoPrTree {
                rows,
                expanded,
                show_all,
            } => {
                let mut total = 0usize;
                let mut hidden = 0usize;
                for r in rows {
                    total += 1; // repo header
                    if expanded.contains(&r.slug) {
                        let (vis, hid) = count_recent_prs(&r.prs, *show_all);
                        total += vis;
                        hidden += hid;
                    }
                }
                if !show_all && hidden > 0 {
                    total += 1; // "Show N older" footer row
                }
                total
            }
            Self::RepoActionsTree { rows, expanded } => rows
                .iter()
                .map(|r| {
                    1 + if expanded.contains(&r.slug) {
                        r.runs.len()
                    } else {
                        0
                    }
                })
                .sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// For the "[ Show N older PRs ]" footer row: how many PRs are
    /// currently hidden by the recency filter, summed across all
    /// EXPANDED repos in a RepoPrTree tab. `None` on non-RepoPrTree
    /// data or when nothing is hidden.
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
            let hid: usize = rows
                .iter()
                .filter(|r| expanded.contains(&r.slug))
                .map(|r| count_recent_prs(&r.prs, false).1)
                .sum();
            if hid == 0 { None } else { Some(hid) }
        } else {
            None
        }
    }
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        // Best-effort whoami — non-fatal, but `mine_only` needs it.
        let me_login = client.whoami().await.ok();
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let parsed_kind = TabKind::from_str(&t.kind).unwrap_or(TabKind::Issues);
            match TabSpec::resolve(t, &cfg.owner) {
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
                        owner: cfg.owner.clone(),
                        repo: None,
                        branch: None,
                        query: None,
                        mine_only: false,
                    },
                    data: TabData::empty_for(parsed_kind),
                    selected: 0,
                    last_fetched: None,
                    last_error: Some(e.to_string()),
                }),
            }
        }
        let mut app = App {
            cfg,
            client,
            tabs,
            active_tab: 0,
            status: String::new(),
            scope_repos: None,
            me_login,
        };
        app.refresh_active().await;
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
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

    /// Resolve the workspace-wide repo scope for `owner`. Applies the
    /// integration-level `repos` allowlist first (task #1092), then
    /// falls back to `scope` = all / recent / explicit. Cached in
    /// `self.scope_repos` so three sibling workspace_* tabs share one
    /// API round-trip.
    pub async fn resolved_scope_repos(&mut self, owner: &str) -> Result<Vec<String>> {
        if let Some(cached) = &self.scope_repos {
            return Ok(cached.clone());
        }
        let hidden: HashSet<String> = self.cfg.hidden_repos.iter().cloned().collect();
        // #1092 — top-level `repos = [...]` bypasses `scope` entirely.
        // Slugs may be bare (resolved against `owner`) or fully-
        // qualified `owner/repo`. Hidden + repo_order still apply.
        let raw: Vec<String> = if !self.cfg.repos.is_empty() {
            self.cfg
                .repos
                .iter()
                .map(|s| qualify_slug(s, owner))
                .collect()
        } else {
            match self.cfg.scope.as_str() {
                "explicit" => self
                    .cfg
                    .explicit_repos
                    .iter()
                    .map(|s| qualify_slug(s, owner))
                    .collect(),
                "recent" => {
                    let activity = self.client.list_owner_repos_with_activity(owner).await?;
                    let cutoff_secs = std::time::SystemTime::now()
                        .checked_sub(std::time::Duration::from_secs(
                            self.cfg.recent_window_days as u64 * 86_400,
                        ))
                        .unwrap_or(std::time::UNIX_EPOCH)
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    activity
                        .into_iter()
                        .filter(|r| !r.archived)
                        .filter(|r| {
                            r.pushed_at
                                .as_deref()
                                .and_then(parse_iso_seconds)
                                .is_some_and(|ts| ts >= cutoff_secs)
                        })
                        .map(|r| r.full_name)
                        .collect()
                }
                _ => self
                    .client
                    .list_owner_repos_with_activity(owner)
                    .await?
                    .into_iter()
                    .filter(|r| !r.archived)
                    .map(|r| r.full_name)
                    .collect(),
            }
        };
        // Subtract hidden — matched against the SHORT slug (repo part)
        // AND the fully-qualified name so users can hide either form.
        let after_hide: Vec<String> = raw
            .into_iter()
            .filter(|s| {
                let short = s.rsplit('/').next().unwrap_or(s);
                !hidden.contains(s) && !hidden.contains(short)
            })
            .collect();
        // Apply repo_order: listed slugs first (in listed order),
        // then the rest in API-return order.
        let ordered: Vec<String> = if self.cfg.repo_order.is_empty() {
            after_hide
        } else {
            let normalized_order: Vec<String> = self
                .cfg
                .repo_order
                .iter()
                .map(|s| qualify_slug(s, owner))
                .collect();
            let order_set: HashSet<&String> = normalized_order.iter().collect();
            let mut head: Vec<String> = normalized_order
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

    pub fn invalidate_scope(&mut self) {
        self.scope_repos = None;
    }

    pub async fn refresh_active(&mut self) {
        let idx = self.active_tab;
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
            TabKind::Issues => {
                let query = spec.query.clone().unwrap_or_default();
                let result = self.client.search(&query, 100).await;
                match result {
                    Ok(items) => {
                        let n = items.len();
                        self.tabs[idx].data = TabData::Issues(items);
                        self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                        self.tabs[idx].last_error = None;
                        self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                        self.status = format!("{name} · {n} items");
                        crate::bridge_client::toast(&format!("{name} · {n} item(s)"));
                    }
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("error: {e}");
                    }
                }
            }
            TabKind::Actions => {
                let (o, r) = match spec.repo.as_deref().and_then(|s| s.split_once('/')) {
                    Some(t) => t,
                    None => {
                        self.tabs[idx].last_error =
                            Some("invalid repo (expected owner/name)".into());
                        self.status = self.tabs[idx].last_error.clone().unwrap();
                        return;
                    }
                };
                let result = self
                    .client
                    .actions_runs(o, r, spec.branch.as_deref(), 30)
                    .await;
                match result {
                    Ok(runs) => {
                        let n = runs.len();
                        self.tabs[idx].data = TabData::Actions(runs);
                        self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                        self.tabs[idx].last_error = None;
                        self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                        self.status = format!("{name} · {n} runs");
                    }
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("error: {e}");
                    }
                }
            }
            TabKind::WorkspaceOpenPrs => {
                let owner = spec.owner.clone();
                let repos = match self.resolved_scope_repos(&owner).await {
                    Ok(r) => r,
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("scope-resolve error: {e}");
                        return;
                    }
                };
                let mut rows = self.client.list_repos_open_prs(&owner, &repos, 25).await;
                self.apply_mine_only(&mut rows, spec.mine_only);
                self.commit_pr_tree(idx, name, rows);
            }
            TabKind::WorkspaceMergedPrs => {
                let owner = spec.owner.clone();
                let repos = match self.resolved_scope_repos(&owner).await {
                    Ok(r) => r,
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("scope-resolve error: {e}");
                        return;
                    }
                };
                let mut rows = self.client.list_repos_closed_prs(&owner, &repos, 25).await;
                // GitHub `state=closed` mixes closed-without-merge with
                // merged; filter to merged only.
                for r in &mut rows {
                    r.prs.retain(|p| p.merged_at.is_some());
                }
                self.apply_mine_only(&mut rows, spec.mine_only);
                self.commit_pr_tree(idx, name, rows);
            }
            TabKind::WorkspaceActions => {
                let owner = spec.owner.clone();
                let repos = match self.resolved_scope_repos(&owner).await {
                    Ok(r) => r,
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("scope-resolve error: {e}");
                        return;
                    }
                };
                let rows = self.client.list_repos_actions(&owner, &repos, 10).await;
                self.commit_actions_tree(idx, name, rows);
            }
        }
    }

    /// #1099 f/u analog — post-fetch filter to author=me. Preserves
    /// tree grouping. Silently no-ops when `me_login` is unknown.
    fn apply_mine_only(&self, rows: &mut Vec<RepoPrs>, mine_only: bool) {
        if !mine_only {
            return;
        }
        let Some(me) = self.me_login.as_deref() else {
            return;
        };
        for r in rows.iter_mut() {
            r.prs
                .retain(|p| p.user.as_ref().is_some_and(|u| u.login == me));
        }
        rows.retain(|r| !r.prs.is_empty() || r.error.is_some());
    }

    fn commit_pr_tree(&mut self, idx: usize, name: String, rows: Vec<RepoPrs>) {
        let n = rows.len();
        let total_prs: usize = rows.iter().map(|r| r.prs.len()).sum();
        let errored: usize = rows.iter().filter(|r| r.error.is_some()).count();
        // Preserve expansion state across refreshes; auto-expand every
        // repo on the very first fetch so PRs are visible without
        // hunting-and-clicking. Matches the bitbucket sibling.
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

    fn commit_actions_tree(&mut self, idx: usize, name: String, rows: Vec<RepoActions>) {
        let n = rows.len();
        let errored: usize = rows.iter().filter(|r| r.error.is_some()).count();
        let prior: HashSet<String> = match &self.tabs[idx].data {
            TabData::RepoActionsTree { expanded, .. } => expanded
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
        self.tabs[idx].data = TabData::RepoActionsTree {
            rows,
            expanded: carry,
        };
        self.tabs[idx].last_fetched = Some(std::time::Instant::now());
        self.tabs[idx].last_error = None;
        let vis = self.tabs[idx].data.len();
        self.tabs[idx].selected = self.tabs[idx].selected.min(vis.saturating_sub(1));
        self.status = if errored > 0 {
            format!("{name} · {n} repos ({errored} errored)")
        } else {
            format!("{name} · {n} repos")
        };
    }

    // ── Tree navigation (workspace_* tabs) ──────────────────────────

    /// Map the active tab's `selected` visible-row index → the
    /// underlying `(repo_slug, Option<child_label>)`. `None` child =
    /// row is a repo header; `Some(label)` = row is a child.
    pub fn tree_focused_row(&self) -> Option<(String, Option<String>)> {
        match &self.active().data {
            TabData::RepoPrTree {
                rows,
                expanded,
                show_all,
            } => {
                let mut idx = self.active().selected;
                for repo in rows {
                    if idx == 0 {
                        return Some((repo.slug.clone(), None));
                    }
                    idx -= 1;
                    if expanded.contains(&repo.slug) {
                        for pr in repo.prs.iter().filter(|p| pr_visible(p, *show_all)) {
                            if idx == 0 {
                                return Some((
                                    repo.slug.clone(),
                                    Some(format!("PR #{}", pr.number)),
                                ));
                            }
                            idx -= 1;
                        }
                    }
                }
                None
            }
            TabData::RepoActionsTree { rows, expanded } => {
                let mut idx = self.active().selected;
                for repo in rows {
                    if idx == 0 {
                        return Some((repo.slug.clone(), None));
                    }
                    idx -= 1;
                    if expanded.contains(&repo.slug) {
                        for run in &repo.runs {
                            if idx == 0 {
                                return Some((
                                    repo.slug.clone(),
                                    Some(format!("run #{}", run.run_number)),
                                ));
                            }
                            idx -= 1;
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Walk a RepoPrTree the same way `tree_focused_row` does, but
    /// return the full PR when the cursor is on a PR row.
    #[allow(dead_code)]
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
                return None;
            }
            idx -= 1;
            if expanded.contains(&repo.slug) {
                for pr in repo.prs.iter().filter(|p| pr_visible(p, show_all)) {
                    if idx == 0 {
                        return Some((repo.slug.clone(), pr.clone()));
                    }
                    idx -= 1;
                }
            }
        }
        None
    }

    /// Walk a RepoActionsTree to find the focused workflow run.
    #[allow(dead_code)]
    pub fn focused_run(&self) -> Option<(String, WorkflowRun)> {
        let (rows, expanded) = match &self.active().data {
            TabData::RepoActionsTree { rows, expanded } => (rows, expanded),
            _ => return None,
        };
        let mut idx = self.active().selected;
        for repo in rows {
            if idx == 0 {
                return None;
            }
            idx -= 1;
            if expanded.contains(&repo.slug) {
                for run in &repo.runs {
                    if idx == 0 {
                        return Some((repo.slug.clone(), run.clone()));
                    }
                    idx -= 1;
                }
            }
        }
        None
    }

    fn tree_expanded_mut(&mut self) -> Option<&mut HashSet<String>> {
        match &mut self.active_mut().data {
            TabData::RepoPrTree { expanded, .. } | TabData::RepoActionsTree { expanded, .. } => {
                Some(expanded)
            }
            _ => None,
        }
    }

    fn tree_repo_slugs(&self) -> Vec<String> {
        match &self.active().data {
            TabData::RepoPrTree { rows, .. } => rows.iter().map(|r| r.slug.clone()).collect(),
            TabData::RepoActionsTree { rows, .. } => rows.iter().map(|r| r.slug.clone()).collect(),
            _ => Vec::new(),
        }
    }

    fn tree_child_count(&self, slug: &str) -> usize {
        match &self.active().data {
            TabData::RepoPrTree { rows, show_all, .. } => rows
                .iter()
                .find(|r| r.slug == slug)
                .map(|r| count_recent_prs(&r.prs, *show_all).0)
                .unwrap_or(0),
            TabData::RepoActionsTree { rows, .. } => rows
                .iter()
                .find(|r| r.slug == slug)
                .map(|r| r.runs.len())
                .unwrap_or(0),
            _ => 0,
        }
    }

    pub fn tree_toggle_focused_repo(&mut self) {
        let Some((slug, child)) = self.tree_focused_row() else {
            return;
        };
        let Some(expanded) = self.tree_expanded_mut() else {
            return;
        };
        if !expanded.insert(slug.clone()) {
            expanded.remove(&slug);
        }
        if child.is_some() {
            // Snap the cursor back to the parent repo header so the
            // visible-index doesn't leak into an unrelated repo.
            self.snap_cursor_to_repo(&slug);
        }
    }

    fn snap_cursor_to_repo(&mut self, slug: &str) {
        let slugs = self.tree_repo_slugs();
        let expanded_snapshot: HashSet<String> = match &self.active().data {
            TabData::RepoPrTree { expanded, .. } | TabData::RepoActionsTree { expanded, .. } => {
                expanded.clone()
            }
            _ => HashSet::new(),
        };
        let mut idx = 0usize;
        for s in &slugs {
            if s == slug {
                self.active_mut().selected = idx;
                return;
            }
            idx += 1;
            if expanded_snapshot.contains(s) {
                idx += self.tree_child_count(s);
            }
        }
    }

    pub fn tree_expand_focused(&mut self) {
        let Some((slug, child)) = self.tree_focused_row() else {
            return;
        };
        if child.is_some() {
            return; // already on a child; no descent
        }
        let already = self.tree_expanded_mut().is_some_and(|e| e.contains(&slug));
        if !already {
            if let Some(expanded) = self.tree_expanded_mut() {
                expanded.insert(slug);
            }
        } else {
            let len = self.active().data.len();
            let s = self.active().selected + 1;
            self.active_mut().selected = s.min(len.saturating_sub(1));
        }
    }

    pub fn tree_collapse_focused(&mut self) {
        let Some((slug, child)) = self.tree_focused_row() else {
            return;
        };
        if child.is_some() {
            if let Some(expanded) = self.tree_expanded_mut() {
                expanded.remove(&slug);
            }
            self.snap_cursor_to_repo(&slug);
            return;
        }
        if let Some(expanded) = self.tree_expanded_mut() {
            expanded.remove(&slug);
        }
    }

    pub fn tree_expand_all(&mut self) {
        let slugs = self.tree_repo_slugs();
        if let Some(expanded) = self.tree_expanded_mut() {
            for s in slugs {
                expanded.insert(s);
            }
        }
    }

    pub fn tree_collapse_all(&mut self) {
        if let Some(expanded) = self.tree_expanded_mut() {
            expanded.clear();
        }
        let len = self.active().data.len();
        self.active_mut().selected = self.active().selected.min(len.saturating_sub(1));
    }

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

    pub async fn tree_cycle_scope(&mut self) -> Result<()> {
        let next = match self.cfg.scope.as_str() {
            "all" => "recent",
            "recent" => "explicit",
            _ => "all",
        };
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

    pub fn tree_reorder_focused(&mut self, delta: i32) -> Result<()> {
        let Some((slug, child)) = self.tree_focused_row() else {
            return Ok(());
        };
        if child.is_some() {
            return Ok(());
        }
        if self.cfg.repo_order.is_empty()
            && let Some(cached) = &self.scope_repos
        {
            self.cfg.repo_order = cached.clone();
        }
        let pos = self.cfg.repo_order.iter().position(|s| s == &slug);
        let Some(pos) = pos else {
            self.cfg.repo_order.push(slug);
            crate::config::save(&self.cfg)?;
            return Ok(());
        };
        let new_pos = (pos as i32 + delta).clamp(0, self.cfg.repo_order.len() as i32 - 1) as usize;
        if new_pos == pos {
            return Ok(());
        }
        let slug = self.cfg.repo_order.remove(pos);
        self.cfg.repo_order.insert(new_pos, slug.clone());
        crate::config::save(&self.cfg)?;
        self.invalidate_scope();
        // Also reorder the visible tree rows in-place so the move
        // shows immediately (before the next refresh completes).
        match &mut self.active_mut().data {
            TabData::RepoPrTree { rows, .. } => {
                if let Some(p) = rows.iter().position(|r| r.slug == slug) {
                    let repo = rows.remove(p);
                    rows.insert(new_pos.min(rows.len()), repo);
                }
            }
            TabData::RepoActionsTree { rows, .. } => {
                if let Some(p) = rows.iter().position(|r| r.slug == slug) {
                    let repo = rows.remove(p);
                    rows.insert(new_pos.min(rows.len()), repo);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn toggle_active_mine_only(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        match tab.spec.kind {
            TabKind::WorkspaceOpenPrs | TabKind::WorkspaceMergedPrs => {
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

    /// Enter/o/y target URL for the focused row.
    fn focused_url(&self) -> Option<String> {
        match &self.active().data {
            TabData::Issues(items) => items
                .get(self.active().selected)
                .map(|i| i.html_url.clone()),
            TabData::Actions(runs) => runs.get(self.active().selected).map(|r| r.html_url.clone()),
            TabData::RepoPrTree { rows, .. } => {
                let (slug, child) = self.tree_focused_row()?;
                let owner = &self.cfg.owner;
                let (o, r) = split_slug_owned(&slug, owner);
                match child {
                    Some(label) => {
                        // "PR #<n>"
                        let id: Option<i64> =
                            label.strip_prefix("PR #").and_then(|s| s.parse().ok());
                        id.and_then(|id| {
                            rows.iter()
                                .find(|r| r.slug == slug)
                                .and_then(|r| r.prs.iter().find(|p| p.number == id))
                                .map(|p| p.html_url.clone())
                        })
                        .or_else(|| Some(format!("https://github.com/{o}/{r}/pulls")))
                    }
                    None => Some(format!("https://github.com/{o}/{r}/pulls")),
                }
            }
            TabData::RepoActionsTree { rows, .. } => {
                let (slug, child) = self.tree_focused_row()?;
                let owner = &self.cfg.owner;
                let (o, r) = split_slug_owned(&slug, owner);
                match child {
                    Some(label) => {
                        let n: Option<i64> =
                            label.strip_prefix("run #").and_then(|s| s.parse().ok());
                        n.and_then(|n| {
                            rows.iter()
                                .find(|r| r.slug == slug)
                                .and_then(|r| r.runs.iter().find(|run| run.run_number == n))
                                .map(|run| run.html_url.clone())
                        })
                        .or_else(|| Some(format!("https://github.com/{o}/{r}/actions")))
                    }
                    None => Some(format!("https://github.com/{o}/{r}/actions")),
                }
            }
        }
    }

    pub fn open_focused(&mut self) {
        let Some(url) = self.focused_url() else {
            self.status = "no URL for this row".into();
            return;
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    pub fn yank_focused_url(&mut self) {
        let Some(url) = self.focused_url() else {
            self.status = "no URL for this row".into();
            return;
        };
        match crate::clipboard::copy(&url) {
            Ok(()) => self.status = format!("copied {url}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// True when the active tab renders as a tree (workspace_* kinds).
    pub fn on_tree(&self) -> bool {
        matches!(
            self.active().data,
            TabData::RepoPrTree { .. } | TabData::RepoActionsTree { .. }
        )
    }

    /// True when the "[ Show N older ]" footer row is under the
    /// cursor. Enter/Space on it flips `show_all` → true.
    pub fn focus_is_show_more_footer(&self) -> bool {
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
        self.active().selected + 1 == self.active().data.len()
    }

    pub fn set_show_all_prs(&mut self, value: bool) {
        if let TabData::RepoPrTree { show_all, .. } = &mut self.active_mut().data {
            *show_all = value;
        }
    }
}

/// Recency filter — return `(visible, hidden)` for a repo's PR list.
pub fn count_recent_prs(prs: &[PullRequest], show_all: bool) -> (usize, usize) {
    if show_all {
        return (prs.len(), 0);
    }
    let mut vis = 0;
    let mut hid = 0;
    for pr in prs {
        if pr_visible(pr, false) {
            vis += 1;
        } else {
            hid += 1;
        }
    }
    (vis, hid)
}

fn pr_visible(pr: &PullRequest, show_all: bool) -> bool {
    if show_all {
        return true;
    }
    // Use the more-recent of merged_at / updated_at so a merged PR
    // that hasn't been touched in weeks still shows up when its
    // merge itself is recent.
    let ts = pr.merged_at.as_deref().or(pr.updated_at.as_deref());
    ts.and_then(hours_since)
        .map(|h| h <= RECENT_WINDOW_HOURS)
        .unwrap_or(true)
}

/// Resolve `slug` to a fully-qualified `owner/repo` — bare names
/// resolve against `default_owner`; already-qualified slugs pass
/// through untouched.
pub fn qualify_slug(slug: &str, default_owner: &str) -> String {
    if slug.contains('/') {
        slug.to_string()
    } else {
        format!("{default_owner}/{slug}")
    }
}

fn split_slug_owned(slug: &str, default_owner: &str) -> (String, String) {
    match slug.split_once('/') {
        Some((o, r)) if !o.is_empty() && !r.is_empty() => (o.to_string(), r.to_string()),
        _ => (default_owner.to_string(), slug.to_string()),
    }
}

pub fn hours_since(iso: &str) -> Option<i64> {
    let then = parse_iso_seconds(iso)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((now - then) / 3600)
}

pub(crate) fn parse_iso_seconds(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let mut date_parts = date.splitn(3, '-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let time_end = rest.find(['.', '+', '-', 'Z']).unwrap_or(rest.len());
    let time = &rest[..time_end];
    let mut time_parts = time.splitn(3, ':');
    let hh: i64 = time_parts.next()?.parse().ok()?;
    let mm: i64 = time_parts.next()?.parse().ok()?;
    let ss: i64 = time_parts.next()?.parse().ok()?;
    // Howard Hinnant's civil_from_days.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let d = day as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;
    use crate::github::User;

    fn issues_tab(name: &str) -> Tab {
        Tab {
            name: name.into(),
            kind: "issues".into(),
            owner: None,
            query: Some("is:open".into()),
            repo: None,
            branch: None,
            mine_only: false,
        }
    }

    #[test]
    fn resolve_issues_tab() {
        let t = issues_tab("Mine");
        let spec = TabSpec::resolve(&t, "acme").unwrap();
        assert_eq!(spec.kind, TabKind::Issues);
        assert_eq!(spec.owner, "acme");
        assert_eq!(spec.query.as_deref(), Some("is:open"));
    }

    #[test]
    fn resolve_workspace_tab_derives_owner_from_default() {
        let t = Tab {
            name: "Open".into(),
            kind: "workspace_open_prs".into(),
            owner: None,
            query: None,
            repo: None,
            branch: None,
            mine_only: false,
        };
        let spec = TabSpec::resolve(&t, "acme").unwrap();
        assert_eq!(spec.kind, TabKind::WorkspaceOpenPrs);
        assert_eq!(spec.owner, "acme");
    }

    #[test]
    fn resolve_workspace_tab_owner_override() {
        let t = Tab {
            name: "Open".into(),
            kind: "workspace_open_prs".into(),
            owner: Some("beta".into()),
            query: None,
            repo: None,
            branch: None,
            mine_only: false,
        };
        let spec = TabSpec::resolve(&t, "acme").unwrap();
        assert_eq!(spec.owner, "beta");
    }

    #[test]
    fn resolve_actions_requires_repo() {
        let t = Tab {
            name: "CI".into(),
            kind: "actions".into(),
            owner: None,
            query: None,
            repo: None,
            branch: None,
            mine_only: false,
        };
        assert!(TabSpec::resolve(&t, "acme").is_err());
    }

    #[test]
    fn parse_iso_seconds_matches_known_timestamps() {
        assert_eq!(parse_iso_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_seconds("2000-01-01T00:00:00Z"), Some(946_684_800));
        assert_eq!(
            parse_iso_seconds("2026-06-27T21:59:39.415826+00:00"),
            Some(1_782_597_579)
        );
    }

    #[test]
    fn count_recent_prs_partitions_by_window() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let recent = iso_from_secs(now - 3600); // 1h ago
        let old = iso_from_secs(now - 3600 * 48); // 2 days ago
        let pr = |ts: String| PullRequest {
            number: 1,
            title: "t".into(),
            html_url: String::new(),
            state: "open".into(),
            draft: false,
            user: None,
            head: None,
            base: None,
            updated_at: Some(ts),
            merged_at: None,
        };
        let prs = vec![pr(recent), pr(old)];
        assert_eq!(count_recent_prs(&prs, false), (1, 1));
        assert_eq!(count_recent_prs(&prs, true), (2, 0));
    }

    #[test]
    fn qualify_slug_leaves_qualified_untouched() {
        assert_eq!(qualify_slug("acme/tool", "def"), "acme/tool");
        assert_eq!(qualify_slug("tool", "def"), "def/tool");
    }

    #[test]
    fn apply_mine_only_filters_and_drops_empties() {
        let app = App {
            cfg: dummy_cfg(),
            client: dummy_client(),
            tabs: Vec::new(),
            active_tab: 0,
            status: String::new(),
            scope_repos: None,
            me_login: Some("alice".into()),
        };
        let alice = PullRequest {
            number: 1,
            title: "a".into(),
            html_url: String::new(),
            state: "open".into(),
            draft: false,
            user: Some(User {
                login: "alice".into(),
            }),
            head: None,
            base: None,
            updated_at: None,
            merged_at: None,
        };
        let bob = PullRequest {
            number: 2,
            title: "b".into(),
            html_url: String::new(),
            state: "open".into(),
            draft: false,
            user: Some(User {
                login: "bob".into(),
            }),
            head: None,
            base: None,
            updated_at: None,
            merged_at: None,
        };
        let mut rows = vec![
            crate::github::RepoPrs {
                slug: "acme/a".into(),
                prs: vec![alice.clone(), bob.clone()],
                error: None,
            },
            crate::github::RepoPrs {
                slug: "acme/b".into(),
                prs: vec![bob.clone()],
                error: None,
            },
        ];
        app.apply_mine_only(&mut rows, true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "acme/a");
        assert_eq!(rows[0].prs.len(), 1);
    }

    fn dummy_cfg() -> Config {
        Config {
            refresh_interval_secs: 60,
            owner: "acme".into(),
            tabs: Vec::new(),
            scope: "recent".into(),
            recent_window_days: 14,
            explicit_repos: Vec::new(),
            hidden_repos: Vec::new(),
            repo_order: Vec::new(),
            repos: Vec::new(),
        }
    }

    fn dummy_client() -> Client {
        Client::new("fake-token").unwrap()
    }

    fn iso_from_secs(secs: i64) -> String {
        // Convert seconds → naive UTC ISO for the test. Reuses
        // chrono (already a dependency) to keep the arithmetic
        // trustworthy — a hand-rolled Hinnant conversion here had
        // subtle bugs on month rollover, and the tests only need a
        // correct round-trip through the sibling's own parser.
        use chrono::TimeZone;
        chrono::Utc
            .timestamp_opt(secs, 0)
            .single()
            .expect("valid unix timestamp")
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
}
