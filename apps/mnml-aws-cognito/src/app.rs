//! App state — per-tab list of Cognito items (User Pools OR Users) +
//! a selection cursor.

use crate::cognito::{self, Item};
use crate::config::{Config, Tab};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub user_pool_id: Option<String>,
    pub user_limit: u32,
    pub region: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match t.kind.as_str() {
            "pools" => Ok(Self {
                kind: "pools".into(),
                user_pool_id: None,
                user_limit: t.user_limit,
                region,
            }),
            "users" => {
                let pool = t.user_pool_id.clone().unwrap_or_default();
                if pool.trim().is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"users\" requires `user_pool_id`", t.name);
                }
                Ok(Self {
                    kind: "users".into(),
                    user_pool_id: Some(pool),
                    user_limit: t.user_limit.max(1),
                    region,
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: ItemsTab,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// When `Some`, the user is typing in the search bar — keys go to
    /// `query_input` instead of navigating the items list. Only active
    /// on `users` tabs (search has no meaning for pool lists).
    pub query_editing: Option<String>,
    /// The committed filter — drives `list-users --filter` on next
    /// refresh. `None` means no filter (show all users).
    pub active_filter: Option<String>,
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t, cfg.region.as_deref())?;
            tabs.push(TabState {
                name: t.name.clone(),
                data: ItemsTab::empty(),
                spec,
            });
        }
        let mut app = App {
            cfg,
            tabs,
            active_tab: 0,
            status: String::new(),
            query_editing: None,
            active_filter: None,
        };
        app.refresh_active();
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
            if self.tabs[idx].data.items.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.data.items.is_empty() {
            return;
        }
        let n = tab.data.items.len() as isize;
        let cur = tab.data.selected as isize;
        let next = (cur + delta).clamp(0, n - 1);
        tab.data.selected = next as usize;
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;

        let result: Result<Vec<Item>> = match spec.kind.as_str() {
            "pools" => cognito::list_user_pools(spec.region.as_deref())
                .map(|ps| ps.into_iter().map(Item::Pool).collect()),
            "users" => {
                let pool = spec
                    .user_pool_id
                    .as_deref()
                    .expect("users tab requires user_pool_id (validated)");
                cognito::list_users(
                    pool,
                    spec.user_limit,
                    spec.region.as_deref(),
                    self.active_filter.as_deref(),
                )
                .map(|us| us.into_iter().map(Item::User).collect())
            }
            _ => unreachable!("validated in TabSpec::resolve"),
        };

        let t = &mut self.tabs[idx];
        t.data.loading = false;
        match result {
            Ok(items) => {
                let count = items.len();
                t.data.items = items;
                t.data.selected = t.data.selected.min(count.saturating_sub(1));
                t.data.last_loaded = Some(Instant::now());
                t.data.last_error = None;
                let kind_label = match spec.kind.as_str() {
                    "pools" => "pools",
                    "users" => "users",
                    _ => "items",
                };
                self.status = format!("{name}: {count} {kind_label}");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return false;
        }
        let idx = self.active_tab;
        let stale = match self.tabs[idx].data.last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].data.loading {
            self.refresh_active();
            true
        } else {
            false
        }
    }

    pub fn drain(&mut self) -> bool {
        false
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    pub fn open_console(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let region = self.active().spec.region.as_deref().unwrap_or("us-east-1");
        let url = match item {
            Item::Pool(p) => format!(
                "https://{region}.console.aws.amazon.com/cognito/v2/idp/user-pools/{}/users?region={region}",
                p.id
            ),
            Item::User(u) => {
                let pool = self.active().spec.user_pool_id.as_deref().unwrap_or("");
                format!(
                    "https://{region}.console.aws.amazon.com/cognito/v2/idp/user-pools/{pool}/users/{}?region={region}",
                    u.username
                )
            }
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `/` — enter search-input mode (users tabs only). Subsequent
    /// keystrokes append to the query until Enter (commit) or Esc
    /// (cancel + clear).
    pub fn enter_search_mode(&mut self) {
        if self.active().spec.kind == "users" {
            self.query_editing = Some(String::new());
            self.status = "search: type a prefix, Enter to apply".into();
        } else {
            self.status = "search only available on users tabs".into();
        }
    }

    pub fn search_input_char(&mut self, c: char) {
        if let Some(buf) = self.query_editing.as_mut() {
            buf.push(c);
        }
    }

    pub fn search_input_backspace(&mut self) {
        if let Some(buf) = self.query_editing.as_mut() {
            buf.pop();
        }
    }

    /// Enter on the search input: build the Cognito filter expression
    /// from the typed query, store it on `active_filter`, then refresh.
    pub fn search_commit(&mut self) {
        let Some(query) = self.query_editing.take() else {
            return;
        };
        let filter = cognito::build_user_filter(&query);
        if filter.is_empty() {
            self.active_filter = None;
            self.status = "search cleared".into();
        } else {
            self.active_filter = Some(filter.clone());
            self.status = format!("filter: {filter}");
        }
        self.refresh_active();
    }

    /// Esc on the search input: cancel without applying. If a filter
    /// was already active, also clear it (vim-style: Esc dismisses
    /// the current view).
    pub fn search_cancel(&mut self) {
        if self.query_editing.take().is_some() {
            self.status = "search cancelled".into();
            return;
        }
        if self.active_filter.take().is_some() {
            self.status = "filter cleared".into();
            self.refresh_active();
        }
    }

    pub fn yank_id(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let (payload, label) = match item {
            Item::Pool(p) => (p.id.clone(), "User Pool ID"),
            Item::User(u) => (u.sub().unwrap_or(&u.username).to_string(), "user sub"),
        };
        match crate::clipboard::copy(&payload) {
            Ok(()) => self.status = format!("copied {label}: {payload}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolve_uses_default_region() {
        let t = Tab {
            name: "x".into(),
            kind: "pools".into(),
            user_pool_id: None,
            user_limit: 60,
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_users_without_pool_id() {
        let t = Tab {
            name: "bad".into(),
            kind: "users".into(),
            user_pool_id: None,
            user_limit: 60,
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }
}
