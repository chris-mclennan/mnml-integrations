//! App state — what's loaded, what's selected, the configured
//! query/endpoint for each tab. Two tab kinds:
//!
//!   - `Issues` — search the Issues API with a single query
//!     string (covers issues AND PRs via `is:pr` filter).
//!   - `Actions` — workflow runs for a single `owner/repo`,
//!     optionally narrowed to a branch.

use crate::config::{Config, Tab};
use crate::github::{Client, Issue, WorkflowRun};
use anyhow::Result;

pub struct App {
    pub cfg: Config,
    pub client: Client,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
}

pub struct TabState {
    pub name: String,
    pub kind: TabKind,
    pub rows: TabRows,
    pub selected: usize,
    pub last_fetched: Option<std::time::Instant>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TabKind {
    Issues {
        query: String,
    },
    Actions {
        repo: String,
        branch: Option<String>,
    },
}

/// Per-tab loaded data. Variant is whatever the kind produced; UI
/// dispatches on it. Keeping kinds separate means we don't trade
/// off field shapes between issues + runs.
pub enum TabRows {
    Issues(Vec<Issue>),
    Actions(Vec<WorkflowRun>),
}

impl TabRows {
    pub fn len(&self) -> usize {
        match self {
            TabRows::Issues(v) => v.len(),
            TabRows::Actions(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        let tabs: Vec<TabState> = cfg.tabs.iter().map(tab_state_from_config).collect();
        let mut app = App {
            cfg,
            client,
            tabs,
            active_tab: 0,
            status: String::new(),
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
        let len = self.active().rows.len();
        if len == 0 {
            return;
        }
        let s = self.active().selected as isize + delta;
        let new = s.clamp(0, len as isize - 1) as usize;
        self.active_mut().selected = new;
    }

    pub async fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let kind = self.tabs[idx].kind.clone();
        self.status = format!("refreshing {}…", self.tabs[idx].name);
        match kind {
            TabKind::Issues { query } => match self.client.search(&query, 100).await {
                Ok(items) => {
                    let n = items.len();
                    let tab_name = self.tabs[idx].name.clone();
                    self.tabs[idx].rows = TabRows::Issues(items);
                    self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                    self.tabs[idx].last_error = None;
                    self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                    self.status = format!("{} · {} items", tab_name, n);
                    crate::bridge_client::toast(&format!("{tab_name} · {n} item(s)"));
                }
                Err(e) => {
                    self.tabs[idx].last_error = Some(e.to_string());
                    self.status = format!("error: {e}");
                }
            },
            TabKind::Actions { repo, branch } => {
                let (owner, name) = match repo.split_once('/') {
                    Some(t) => t,
                    None => {
                        self.tabs[idx].last_error =
                            Some(format!("invalid repo: {repo} (expected owner/name)"));
                        self.status = self.tabs[idx].last_error.clone().unwrap();
                        return;
                    }
                };
                match self
                    .client
                    .actions_runs(owner, name, branch.as_deref(), 30)
                    .await
                {
                    Ok(runs) => {
                        let n = runs.len();
                        self.tabs[idx].rows = TabRows::Actions(runs);
                        self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                        self.tabs[idx].last_error = None;
                        self.tabs[idx].selected = self.tabs[idx].selected.min(n.saturating_sub(1));
                        self.status = format!("{} · {} runs", self.tabs[idx].name, n);
                    }
                    Err(e) => {
                        self.tabs[idx].last_error = Some(e.to_string());
                        self.status = format!("error: {e}");
                    }
                }
            }
        }
    }

    pub fn open_focused(&mut self) {
        let Some((url, label)) = self.focused_url() else {
            return;
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {label} in browser"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` on a focused row — copy the same URL `Enter`/`o` would
    /// open to the OS clipboard. Restores the pre-split mnml
    /// `github.copy_selected_pr_url` / `_url` palette commands.
    pub fn yank_focused_url(&mut self) {
        let Some((url, _)) = self.focused_url() else {
            return;
        };
        match crate::clipboard::copy(&url) {
            Ok(()) => self.status = format!("copied {url}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    fn focused_url(&self) -> Option<(String, String)> {
        match &self.active().rows {
            TabRows::Issues(items) => items.get(self.active().selected).map(|i| {
                let badge = format!("{}#{}", i.repo_short(), i.number);
                (i.html_url.clone(), badge)
            }),
            TabRows::Actions(runs) => runs.get(self.active().selected).map(|r| {
                let label = format!("{} #{}", r.name.as_deref().unwrap_or("run"), r.run_number);
                (r.html_url.clone(), label)
            }),
        }
    }
}

fn tab_state_from_config(t: &Tab) -> TabState {
    let kind = match t.kind.as_str() {
        "actions" => TabKind::Actions {
            repo: t.repo.clone().unwrap_or_default(),
            branch: t.branch.clone(),
        },
        _ => TabKind::Issues {
            query: t.query.clone().unwrap_or_default(),
        },
    };
    let rows = match &kind {
        TabKind::Issues { .. } => TabRows::Issues(Vec::new()),
        TabKind::Actions { .. } => TabRows::Actions(Vec::new()),
    };
    TabState {
        name: t.name.clone(),
        kind,
        rows,
        selected: 0,
        last_fetched: None,
        last_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_state_from_config_issues_default() {
        let t = Tab {
            name: "Mine".into(),
            kind: "issues".into(),
            query: Some("is:open".into()),
            repo: None,
            branch: None,
        };
        let s = tab_state_from_config(&t);
        assert!(matches!(s.kind, TabKind::Issues { .. }));
        assert!(matches!(s.rows, TabRows::Issues(_)));
    }

    #[test]
    fn tab_state_from_config_actions() {
        let t = Tab {
            name: "CI".into(),
            kind: "actions".into(),
            query: None,
            repo: Some("owner/name".into()),
            branch: Some("main".into()),
        };
        let s = tab_state_from_config(&t);
        match &s.kind {
            TabKind::Actions { repo, branch } => {
                assert_eq!(repo, "owner/name");
                assert_eq!(branch.as_deref(), Some("main"));
            }
            _ => panic!("expected Actions kind"),
        }
        assert!(matches!(s.rows, TabRows::Actions(_)));
    }
}
