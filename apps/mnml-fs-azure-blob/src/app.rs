//! App state — tabs (Accounts | Containers | Blobs), drill-down
//! stack, selection. The Azure CLI calls happen on a worker thread;
//! the App polls a channel each tick to drain results.

use crate::azure_blob::{self, Entry};
use crate::config::{Config, TabConfig, TabKind};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

/// What a tab is currently showing. Mutates as the user drills in /
/// out. The initial value comes from the tab's TabConfig.
#[derive(Debug, Clone)]
pub enum View {
    /// Listing storage accounts.
    Accounts,
    /// Listing containers in `account`.
    Containers { account: String },
    /// Listing blobs in `account` / `container` under `prefix`.
    Blobs {
        account: String,
        container: String,
        prefix: String,
    },
}

#[derive(Debug)]
pub struct TabState {
    pub name: String,
    pub view: View,
    /// Stack of past views to pop on Backspace / `h`.
    pub view_stack: Vec<View>,
    pub items: Vec<Entry>,
    pub selected: usize,
    pub last_error: Option<String>,
    pub loading: bool,
    pub pending: Option<Receiver<AzureEvent>>,
}

#[derive(Debug, Clone)]
pub enum AzureEvent {
    Listed(Vec<Entry>),
    Failed(String),
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// Pending confirmation prompt — set when the user presses
    /// `d` to delete. The UI surfaces "delete <blob>? y/N" and the
    /// next key press resolves it.
    pub pending_confirm: Option<PendingConfirm>,
}

#[derive(Debug, Clone)]
pub enum PendingConfirm {
    Delete {
        account: String,
        container: String,
        blob: String,
    },
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs: Vec<TabState> = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            tabs.push(tab_from_config(t));
        }
        let mut app = App {
            cfg,
            tabs,
            active_tab: 0,
            status: String::new(),
            pending_confirm: None,
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
        if idx < self.tabs.len() && idx != self.active_tab {
            self.active_tab = idx;
            // Only fetch on first activation; subsequent switches
            // reuse the cached listing until the user hits `r`.
            if self.tabs[idx].items.is_empty() && !self.tabs[idx].loading {
                self.refresh_active();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.items.is_empty() {
            return;
        }
        let n = tab.items.len() as isize;
        let next = (tab.selected as isize + delta).clamp(0, n - 1) as usize;
        tab.selected = next;
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let view = self.tabs[idx].view.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("{name} · loading…");
        let (tx, rx) = channel();
        thread::spawn(move || {
            let result = match &view {
                View::Accounts => azure_blob::list_accounts(),
                View::Containers { account } => azure_blob::list_containers(account),
                View::Blobs {
                    account,
                    container,
                    prefix,
                } => azure_blob::list_blobs(account, container, prefix),
            };
            let _ = match result {
                Ok(items) => tx.send(AzureEvent::Listed(items)),
                Err(e) => tx.send(AzureEvent::Failed(e.to_string())),
            };
        });
        let t = &mut self.tabs[idx];
        t.loading = true;
        t.last_error = None;
        t.pending = Some(rx);
    }

    /// Drain background channels — call from the main loop each
    /// tick. Returns true if anything changed (redraw).
    pub fn drain(&mut self) -> bool {
        let mut any = false;
        for tab in self.tabs.iter_mut() {
            let Some(rx) = tab.pending.take() else {
                continue;
            };
            let mut done = false;
            loop {
                match rx.try_recv() {
                    Ok(AzureEvent::Listed(items)) => {
                        any = true;
                        let n = items.len();
                        tab.items = items;
                        tab.loading = false;
                        tab.last_error = None;
                        // Keep selection in range across re-list.
                        if tab.selected >= tab.items.len() {
                            tab.selected = tab.items.len().saturating_sub(1);
                        }
                        done = true;
                        self.status =
                            format!("{} · {} · {n} entries", tab.name, describe_view(&tab.view));
                    }
                    Ok(AzureEvent::Failed(e)) => {
                        any = true;
                        tab.last_error = Some(e.clone());
                        tab.loading = false;
                        done = true;
                        self.status = format!("error: {e}");
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
            if !done {
                tab.pending = Some(rx);
            }
        }
        any
    }

    /// `Enter` — drill into the focused row.
    pub fn enter_focused(&mut self) {
        let Some(entry) = self.focused_entry().cloned() else {
            return;
        };
        match entry {
            Entry::Account(a) => {
                let tab = self.active_mut();
                tab.view_stack.push(tab.view.clone());
                tab.view = View::Containers { account: a.name };
                tab.selected = 0;
                tab.items.clear();
                self.refresh_active();
            }
            Entry::Container(c) => {
                let tab = self.active_mut();
                let account = match &tab.view {
                    View::Containers { account } => account.clone(),
                    _ => return,
                };
                tab.view_stack.push(tab.view.clone());
                tab.view = View::Blobs {
                    account,
                    container: c.name,
                    prefix: String::new(),
                };
                tab.selected = 0;
                tab.items.clear();
                self.refresh_active();
            }
            Entry::Prefix(p) => {
                let tab = self.active_mut();
                let new_view = match &tab.view {
                    View::Blobs {
                        account,
                        container,
                        prefix,
                    } => View::Blobs {
                        account: account.clone(),
                        container: container.clone(),
                        prefix: format!("{prefix}{}", p.name),
                    },
                    _ => return,
                };
                tab.view_stack.push(tab.view.clone());
                tab.view = new_view;
                tab.selected = 0;
                tab.items.clear();
                self.refresh_active();
            }
            Entry::Blob(b) => {
                let tab = self.active();
                let (account, container) = match &tab.view {
                    View::Blobs {
                        account, container, ..
                    } => (account.clone(), container.clone()),
                    _ => return,
                };
                let dest = cache_path_for(&account, &container, &b.full_name);
                self.status = format!("downloading {}…", b.full_name);
                match azure_blob::download(&account, &container, &b.full_name, &dest) {
                    Ok(path) => {
                        let path_str = path.to_string_lossy().to_string();
                        self.status = format!("downloaded → {path_str}");
                    }
                    Err(e) => self.status = format!("download failed: {e}"),
                }
            }
        }
    }

    /// Backspace / `h` — go up one view level.
    pub fn pop_view(&mut self) {
        let tab = self.active_mut();
        let Some(prev) = tab.view_stack.pop() else {
            return;
        };
        tab.view = prev;
        tab.selected = 0;
        tab.items.clear();
        self.refresh_active();
    }

    /// `y` — yank a canonical https URI for the focused entry.
    pub fn yank_uri(&mut self) {
        let Some(uri) = self.focused_uri() else {
            self.status = "no URI for this row".into();
            return;
        };
        match crate::clipboard::copy(&uri) {
            Ok(()) => self.status = format!("copied {uri}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `Y` — yank a SAS-signed https URL (5-min TTL, read-only) for
    /// the focused blob. No-op on non-blob rows.
    pub fn yank_presigned(&mut self) {
        let Some(entry) = self.focused_entry().cloned() else {
            return;
        };
        let Entry::Blob(b) = entry else {
            self.status = "SAS only applies to blobs".into();
            return;
        };
        let tab = self.active();
        let (account, container) = match &tab.view {
            View::Blobs {
                account, container, ..
            } => (account.clone(), container.clone()),
            _ => return,
        };
        match azure_blob::presign(&account, &container, &b.full_name) {
            Ok(url) => match crate::clipboard::copy(&url) {
                Ok(()) => self.status = format!("copied SAS (5 min) {url}"),
                Err(e) => self.status = format!("copy failed: {e}"),
            },
            Err(e) => self.status = format!("SAS generation failed: {e}"),
        }
    }

    /// `o` — open the Azure portal URL for the focused row.
    pub fn open_portal(&mut self) {
        let (account, container, blob) = self.focused_portal_target();
        let url = azure_blob::portal_url(account.as_deref(), container.as_deref(), blob.as_deref());
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `d` — arm a delete confirmation for the focused row (blobs
    /// only). The UI surfaces the prompt; the next `y` confirms,
    /// any other key cancels.
    pub fn arm_delete(&mut self) {
        let Some(entry) = self.focused_entry().cloned() else {
            return;
        };
        let Entry::Blob(b) = entry else {
            self.status = "delete only applies to blobs".into();
            return;
        };
        let tab = self.active();
        let (account, container) = match &tab.view {
            View::Blobs {
                account, container, ..
            } => (account.clone(), container.clone()),
            _ => return,
        };
        self.status = format!(
            "delete {}? press `y` to confirm, any other key to cancel",
            b.full_name
        );
        self.pending_confirm = Some(PendingConfirm::Delete {
            account,
            container,
            blob: b.full_name,
        });
    }

    /// Resolve a pending confirm — the keys layer dispatches this
    /// when the user presses `y` after `arm_delete`.
    pub fn confirm(&mut self) {
        let Some(pending) = self.pending_confirm.take() else {
            return;
        };
        match pending {
            PendingConfirm::Delete {
                account,
                container,
                blob,
            } => match azure_blob::delete(&account, &container, &blob) {
                Ok(()) => {
                    self.status = format!("deleted {blob}");
                    self.refresh_active();
                }
                Err(e) => self.status = format!("delete failed: {e}"),
            },
        }
    }

    /// Any non-`y` key after `arm_delete` cancels the pending
    /// confirm.
    pub fn cancel_confirm(&mut self) {
        if self.pending_confirm.take().is_some() {
            self.status = "cancelled".into();
        }
    }

    fn focused_entry(&self) -> Option<&Entry> {
        let tab = self.active();
        tab.items.get(tab.selected)
    }

    fn focused_uri(&self) -> Option<String> {
        let tab = self.active();
        let entry = tab.items.get(tab.selected)?;
        Some(match entry {
            Entry::Account(a) => azure_blob::https_url(&a.name, None, None),
            Entry::Container(c) => match &tab.view {
                View::Containers { account } => azure_blob::https_url(account, Some(&c.name), None),
                _ => return None,
            },
            Entry::Prefix(p) => match &tab.view {
                View::Blobs {
                    account,
                    container,
                    prefix,
                } => azure_blob::https_url(
                    account,
                    Some(container),
                    Some(&format!("{prefix}{}", p.name)),
                ),
                _ => return None,
            },
            Entry::Blob(b) => match &tab.view {
                View::Blobs {
                    account, container, ..
                } => azure_blob::https_url(account, Some(container), Some(&b.full_name)),
                _ => return None,
            },
        })
    }

    fn focused_portal_target(&self) -> (Option<String>, Option<String>, Option<String>) {
        let tab = self.active();
        let entry = tab.items.get(tab.selected);
        match (&tab.view, entry) {
            (View::Accounts, Some(Entry::Account(a))) => (Some(a.name.clone()), None, None),
            (View::Containers { account }, Some(Entry::Container(c))) => {
                (Some(account.clone()), Some(c.name.clone()), None)
            }
            (
                View::Blobs {
                    account, container, ..
                },
                Some(Entry::Blob(b)),
            ) => (
                Some(account.clone()),
                Some(container.clone()),
                Some(b.full_name.clone()),
            ),
            (
                View::Blobs {
                    account, container, ..
                },
                _,
            ) => (Some(account.clone()), Some(container.clone()), None),
            (View::Containers { account }, _) => (Some(account.clone()), None, None),
            _ => (None, None, None),
        }
    }
}

fn tab_from_config(t: &TabConfig) -> TabState {
    let view = match t.kind {
        TabKind::Accounts => View::Accounts,
        TabKind::Containers => View::Containers {
            account: t.account.clone().unwrap_or_default(),
        },
        TabKind::Blobs => {
            let mut prefix = t.prefix.clone().unwrap_or_default();
            // Normalize — non-empty prefixes need trailing `/` so the
            // delimiter-based list works correctly.
            if !prefix.is_empty() && !prefix.ends_with('/') {
                prefix.push('/');
            }
            View::Blobs {
                account: t.account.clone().unwrap_or_default(),
                container: t.container.clone().unwrap_or_default(),
                prefix,
            }
        }
    };
    TabState {
        name: t.name.clone(),
        view,
        view_stack: Vec::new(),
        items: Vec::new(),
        selected: 0,
        last_error: None,
        loading: false,
        pending: None,
    }
}

fn describe_view(v: &View) -> String {
    match v {
        View::Accounts => "accounts".to_string(),
        View::Containers { account } => format!("{account} / containers"),
        View::Blobs {
            account,
            container,
            prefix,
        } => {
            if prefix.is_empty() {
                format!("{account}/{container}/")
            } else {
                format!("{account}/{container}/{prefix}")
            }
        }
    }
}

fn cache_path_for(account: &str, container: &str, blob: &str) -> PathBuf {
    let mut p = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("mnml-fs-azure-blob");
    p.push(account);
    p.push(container);
    // The blob name may contain `/`s — that's fine, PathBuf joins them.
    p.push(blob);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{TabConfig, TabKind};

    #[test]
    fn blobs_prefix_normalization_adds_trailing_slash() {
        let t = tab_from_config(&TabConfig {
            name: "x".into(),
            kind: TabKind::Blobs,
            account: Some("a".into()),
            container: Some("c".into()),
            prefix: Some("logs/2026".into()),
        });
        if let View::Blobs { prefix, .. } = t.view {
            assert_eq!(prefix, "logs/2026/");
        } else {
            panic!("expected View::Blobs");
        }
    }

    #[test]
    fn blobs_prefix_preserves_trailing_slash() {
        let t = tab_from_config(&TabConfig {
            name: "x".into(),
            kind: TabKind::Blobs,
            account: Some("a".into()),
            container: Some("c".into()),
            prefix: Some("logs/2026/".into()),
        });
        if let View::Blobs { prefix, .. } = t.view {
            assert_eq!(prefix, "logs/2026/");
        } else {
            panic!("expected View::Blobs");
        }
    }

    #[test]
    fn accounts_tab_has_no_account() {
        let t = tab_from_config(&TabConfig {
            name: "all".into(),
            kind: TabKind::Accounts,
            account: None,
            container: None,
            prefix: None,
        });
        assert!(matches!(t.view, View::Accounts));
    }

    #[test]
    fn cache_path_has_account_container_blob() {
        let p = cache_path_for("myacct", "logs", "a/b/c.txt");
        let s = p.to_string_lossy();
        assert!(s.contains("mnml-fs-azure-blob"));
        assert!(s.contains("myacct"));
        assert!(s.contains("logs"));
        assert!(s.ends_with("a/b/c.txt"));
    }
}
