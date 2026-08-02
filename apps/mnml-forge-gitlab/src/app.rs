//! App state — resolved tab specs, loaded rows, status string.

use crate::config::{Config, Tab};
use crate::gitlab::{Client, MergeRequest, Pipeline};
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    MergeRequests,
    Pipelines,
}

impl TabKind {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "merge_requests" => Ok(Self::MergeRequests),
            "pipelines" => Ok(Self::Pipelines),
            other => Err(anyhow::anyhow!("unknown tab kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TabData {
    MergeRequests(Vec<MergeRequest>),
    Pipelines(Vec<Pipeline>),
}

impl TabData {
    pub fn empty_for(kind: TabKind) -> Self {
        match kind {
            TabKind::MergeRequests => Self::MergeRequests(Vec::new()),
            TabKind::Pipelines => Self::Pipelines(Vec::new()),
        }
    }
    pub fn len(&self) -> usize {
        match self {
            Self::MergeRequests(v) => v.len(),
            Self::Pipelines(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct App {
    pub cfg: Config,
    pub client: Client,
    /// Authenticated user's GitLab `id`, resolved at startup via
    /// `/user`. Drives `mode = mine / reviewing`. `None` ⇒ whoami
    /// failed; auto-mode tabs surface that error on first refresh.
    pub me_id: Option<i64>,
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
    pub project: Option<String>,
    pub state: String,
    pub mode: Option<String>,
    pub ref_name: Option<String>,
}

impl TabSpec {
    pub fn resolve(tab: &Tab) -> Result<Self> {
        let kind = TabKind::from_str(&tab.kind)?;
        match kind {
            TabKind::MergeRequests => {
                if let Some(mode) = &tab.mode {
                    if mode != "mine" && mode != "reviewing" {
                        return Err(anyhow::anyhow!("unknown mode: {mode}"));
                    }
                } else if tab.project.is_none() {
                    return Err(anyhow::anyhow!("MR tab needs `mode` or `project`"));
                }
                Ok(Self {
                    kind,
                    project: tab.project.clone(),
                    state: tab.state.clone(),
                    mode: tab.mode.clone(),
                    ref_name: None,
                })
            }
            TabKind::Pipelines => {
                let project = tab
                    .project
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("pipelines tab needs `project`"))?;
                Ok(Self {
                    kind,
                    project: Some(project),
                    state: String::new(),
                    mode: None,
                    ref_name: tab.ref_name.clone(),
                })
            }
        }
    }
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        let (me_id, whoami_err) = match client.whoami().await {
            Ok(u) => (Some(u.id), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let parsed_kind = TabKind::from_str(&t.kind).unwrap_or(TabKind::MergeRequests);
            match TabSpec::resolve(t) {
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
                        project: None,
                        state: t.state.clone(),
                        mode: None,
                        ref_name: None,
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
            .map(|e| format!("/user failed: {e}"))
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
            TabKind::MergeRequests => {
                let result = match spec.mode.as_deref() {
                    Some("mine") => match self.me_id {
                        Some(id) => {
                            self.client
                                .merge_requests_by_person("author_id", id, &spec.state, 50)
                                .await
                        }
                        None => Err(anyhow::anyhow!(
                            "mode=\"mine\" needs /user to succeed at startup"
                        )),
                    },
                    Some("reviewing") => match self.me_id {
                        Some(id) => {
                            self.client
                                .merge_requests_by_person("reviewer_id", id, &spec.state, 50)
                                .await
                        }
                        None => Err(anyhow::anyhow!(
                            "mode=\"reviewing\" needs /user to succeed at startup"
                        )),
                    },
                    _ => {
                        let proj = spec.project.as_deref().unwrap_or("");
                        self.client
                            .merge_requests_project(proj, &spec.state, 50)
                            .await
                    }
                };
                self.commit_mr_refresh(idx, name, result);
            }
            TabKind::Pipelines => {
                let proj = spec.project.as_deref().unwrap_or("");
                let result = self
                    .client
                    .pipelines(proj, spec.ref_name.as_deref(), 30)
                    .await;
                self.commit_pipeline_refresh(idx, name, result);
            }
        }
    }

    fn commit_mr_refresh(&mut self, idx: usize, name: String, result: Result<Vec<MergeRequest>>) {
        match result {
            Ok(mrs) => {
                let n = mrs.len();
                self.tabs[idx].data = TabData::MergeRequests(mrs);
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                self.status = format!("{name} · {n} MRs");
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    fn commit_pipeline_refresh(
        &mut self,
        idx: usize,
        name: String,
        result: Result<Vec<Pipeline>>,
    ) {
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
    /// Restores the pre-split `gitlab.copy_selected_*` commands.
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
            TabData::MergeRequests(mrs) => mrs.get(tab.selected).map(|m| m.web_url.clone()),
            TabData::Pipelines(ps) => ps.get(tab.selected).map(|p| p.web_url.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> Tab {
        Tab {
            name: name.into(),
            kind: "merge_requests".into(),
            project: None,
            state: "opened".into(),
            mode: None,
            ref_name: None,
        }
    }

    #[test]
    fn resolve_project_mr_tab() {
        let mut tab = t("repo");
        tab.project = Some("group/project".into());
        let spec = TabSpec::resolve(&tab).unwrap();
        assert_eq!(spec.kind, TabKind::MergeRequests);
        assert_eq!(spec.project.as_deref(), Some("group/project"));
    }

    #[test]
    fn resolve_mine_mode() {
        let mut tab = t("mine");
        tab.mode = Some("mine".into());
        let spec = TabSpec::resolve(&tab).unwrap();
        assert_eq!(spec.mode.as_deref(), Some("mine"));
        assert!(spec.project.is_none());
    }

    #[test]
    fn resolve_mr_without_mode_or_project_errors() {
        let tab = t("bad");
        let err = TabSpec::resolve(&tab).unwrap_err();
        assert!(err.to_string().contains("mode") || err.to_string().contains("project"));
    }

    #[test]
    fn resolve_pipelines_kind_with_project() {
        let mut tab = t("CI");
        tab.kind = "pipelines".into();
        tab.project = Some("group/project".into());
        let spec = TabSpec::resolve(&tab).unwrap();
        assert_eq!(spec.kind, TabKind::Pipelines);
    }

    #[test]
    fn resolve_pipelines_without_project_errors() {
        let mut tab = t("CI");
        tab.kind = "pipelines".into();
        let err = TabSpec::resolve(&tab).unwrap_err();
        assert!(err.to_string().contains("project"));
    }

    #[test]
    fn resolve_unknown_mode_errors() {
        let mut tab = t("bad");
        tab.mode = Some("garbage".into());
        let err = TabSpec::resolve(&tab).unwrap_err();
        assert!(err.to_string().contains("garbage"));
    }

    #[test]
    fn resolve_unknown_kind_errors() {
        let mut tab = t("bad");
        tab.kind = "garbage".into();
        let err = TabSpec::resolve(&tab).unwrap_err();
        assert!(err.to_string().contains("garbage"));
    }
}
