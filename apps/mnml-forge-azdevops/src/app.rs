//! App state — resolved tab specs, loaded rows, status string.

use crate::azdevops::{Build, Client, PullRequest};
use crate::config::{Config, Tab};
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    PullRequests,
    Builds,
}

impl TabKind {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "pull_requests" => Ok(Self::PullRequests),
            "builds" => Ok(Self::Builds),
            other => Err(anyhow::anyhow!("unknown tab kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TabData {
    PullRequests(Vec<PullRequest>),
    Builds(Vec<Build>),
}

impl TabData {
    pub fn empty_for(kind: TabKind) -> Self {
        match kind {
            TabKind::PullRequests => Self::PullRequests(Vec::new()),
            TabKind::Builds => Self::Builds(Vec::new()),
        }
    }
    pub fn len(&self) -> usize {
        match self {
            Self::PullRequests(v) => v.len(),
            Self::Builds(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct App {
    pub cfg: Config,
    pub client: Client,
    /// Authenticated user's GUID, resolved at startup via
    /// `connectionData`. Drives `mode = "mine" / "reviewing"`. `None`
    /// ⇒ resolution failed (auto-mode tabs surface that as their
    /// per-tab error on first refresh).
    pub me_id: Option<String>,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: TabData,
    pub selected: usize,
    pub last_fetched: Option<std::time::Instant>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: TabKind,
    pub org: String,
    pub project: String,
    pub repo: Option<String>,
    /// PR status filter — only meaningful for `PullRequests`.
    pub state: String,
    /// Mode for `PullRequests`: None ⇒ per-repo, Some("mine") /
    /// Some("reviewing") ⇒ project-spanning by-person.
    pub mode: Option<String>,
    /// Build narrowers (only meaningful for `Builds`).
    pub branch: Option<String>,
    pub definition: Option<i64>,
}

impl TabSpec {
    pub fn resolve(tab: &Tab, default_org: &str, default_project: Option<&str>) -> Result<Self> {
        let kind = TabKind::from_str(&tab.kind)?;
        let org = tab.org.clone().unwrap_or_else(|| default_org.to_string());
        let project = tab
            .project
            .clone()
            .or_else(|| default_project.map(str::to_string))
            .ok_or_else(|| anyhow::anyhow!("no project resolved"))?;
        match kind {
            TabKind::PullRequests => {
                if let Some(mode) = &tab.mode {
                    if mode != "mine" && mode != "reviewing" {
                        return Err(anyhow::anyhow!("unknown mode: {mode}"));
                    }
                } else if tab.repo.is_none() {
                    return Err(anyhow::anyhow!("PR tab needs `mode` or `repo`"));
                }
                Ok(Self {
                    kind,
                    org,
                    project,
                    repo: tab.repo.clone(),
                    state: tab.state.clone(),
                    mode: tab.mode.clone(),
                    branch: None,
                    definition: None,
                })
            }
            TabKind::Builds => Ok(Self {
                kind,
                org,
                project,
                repo: tab.repo.clone(),
                state: String::new(),
                mode: None,
                branch: tab.branch.clone(),
                definition: tab.definition,
            }),
        }
    }
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        // Resolve current-user GUID once. Failure is non-fatal —
        // non-auto tabs still work; auto-mode tabs surface the error
        // on their first refresh.
        let (me_id, whoami_err) = match client.connection_data(&cfg.org).await {
            Ok(cd) => (Some(cd.authenticated_user.id), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let parsed_kind = TabKind::from_str(&t.kind).unwrap_or(TabKind::PullRequests);
            match TabSpec::resolve(t, &cfg.org, cfg.project.as_deref()) {
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
                        org: cfg.org.clone(),
                        project: cfg.project.clone().unwrap_or_default(),
                        repo: None,
                        state: t.state.clone(),
                        mode: None,
                        branch: None,
                        definition: None,
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
            .map(|e| format!("connectionData failed: {e}"))
            .unwrap_or_default();
        let mut app = App {
            cfg,
            client,
            me_id,
            tabs,
            active_tab: 0,
            status,
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

    pub async fn refresh_active(&mut self) {
        let idx = self.active_tab;
        if self.tabs[idx].last_error.is_some() && self.tabs[idx].data.is_empty() {
            // Resolution-time error — surface it and bail. User can
            // edit the config + reload.
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
                let result = match spec.mode.as_deref() {
                    Some("mine") => match self.me_id.as_deref() {
                        Some(id) => {
                            self.client
                                .pull_requests_by_person(
                                    &spec.org,
                                    &spec.project,
                                    "searchCriteria.creatorId",
                                    id,
                                    &spec.state,
                                    50,
                                )
                                .await
                        }
                        None => Err(anyhow::anyhow!(
                            "mode=\"mine\" needs User Profile: Read scope on the PAT"
                        )),
                    },
                    Some("reviewing") => match self.me_id.as_deref() {
                        Some(id) => {
                            self.client
                                .pull_requests_by_person(
                                    &spec.org,
                                    &spec.project,
                                    "searchCriteria.reviewerId",
                                    id,
                                    &spec.state,
                                    50,
                                )
                                .await
                        }
                        None => Err(anyhow::anyhow!(
                            "mode=\"reviewing\" needs User Profile: Read scope on the PAT"
                        )),
                    },
                    _ => {
                        let repo = spec.repo.as_deref().unwrap_or("");
                        self.client
                            .pull_requests_repo(&spec.org, &spec.project, repo, &spec.state, 50)
                            .await
                    }
                };
                self.commit_pr_refresh(idx, name, result);
            }
            TabKind::Builds => {
                let result = self
                    .client
                    .builds(
                        &spec.org,
                        &spec.project,
                        spec.repo.as_deref(),
                        spec.branch.as_deref(),
                        spec.definition,
                        50,
                    )
                    .await;
                self.commit_build_refresh(idx, name, result);
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

    fn commit_build_refresh(&mut self, idx: usize, name: String, result: Result<Vec<Build>>) {
        match result {
            Ok(bs) => {
                let n = bs.len();
                self.tabs[idx].data = TabData::Builds(bs);
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                self.status = format!("{name} · {n} builds");
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
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

    /// `y` on a focused row — copy the URL to the OS clipboard.
    /// Restores the pre-split `azdevops.copy_selected_*` commands.
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

    fn focused_url(&self) -> Option<String> {
        let tab = self.active();
        match &tab.data {
            TabData::PullRequests(prs) => prs
                .get(tab.selected)
                .map(|p| p.web_url(&tab.spec.org, &tab.spec.project)),
            TabData::Builds(bs) => bs
                .get(tab.selected)
                .and_then(|b| b.web_url().map(str::to_string)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> Tab {
        Tab {
            name: name.into(),
            kind: "pull_requests".into(),
            org: None,
            project: None,
            repo: None,
            state: "active".into(),
            mode: None,
            definition: None,
            branch: None,
        }
    }

    #[test]
    fn resolve_repo_tab_uses_default_org_and_project() {
        let mut tab = t("repo");
        tab.repo = Some("myrepo".into());
        let spec = TabSpec::resolve(&tab, "orgA", Some("proj1")).unwrap();
        assert_eq!(spec.org, "orgA");
        assert_eq!(spec.project, "proj1");
        assert_eq!(spec.repo.as_deref(), Some("myrepo"));
        assert_eq!(spec.state, "active");
    }

    #[test]
    fn resolve_tab_org_overrides_default() {
        let mut tab = t("repo");
        tab.org = Some("orgX".into());
        tab.repo = Some("r".into());
        let spec = TabSpec::resolve(&tab, "default", Some("p")).unwrap();
        assert_eq!(spec.org, "orgX");
    }

    #[test]
    fn resolve_mine_mode() {
        let mut tab = t("mine");
        tab.mode = Some("mine".into());
        let spec = TabSpec::resolve(&tab, "o", Some("p")).unwrap();
        assert_eq!(spec.mode.as_deref(), Some("mine"));
    }

    #[test]
    fn resolve_pr_without_mode_or_repo_errors() {
        let tab = t("bad");
        let err = TabSpec::resolve(&tab, "o", Some("p")).unwrap_err();
        assert!(err.to_string().contains("mode") || err.to_string().contains("repo"));
    }

    #[test]
    fn resolve_no_project_errors() {
        let mut tab = t("x");
        tab.repo = Some("r".into());
        let err = TabSpec::resolve(&tab, "o", None).unwrap_err();
        assert!(err.to_string().contains("project"));
    }

    #[test]
    fn resolve_unknown_mode_errors() {
        let mut tab = t("bad");
        tab.mode = Some("garbage".into());
        let err = TabSpec::resolve(&tab, "o", Some("p")).unwrap_err();
        assert!(err.to_string().contains("garbage"));
    }

    #[test]
    fn resolve_builds_kind_without_repo() {
        let mut tab = t("b");
        tab.kind = "builds".into();
        let spec = TabSpec::resolve(&tab, "o", Some("p")).unwrap();
        assert_eq!(spec.kind, TabKind::Builds);
        assert!(spec.repo.is_none());
    }
}
