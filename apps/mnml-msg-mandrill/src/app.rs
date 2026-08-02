//! App state — per-tab item lists + a selection cursor. Items are
//! a 4-variant enum because each tab kind has a distinct shape.

use crate::config::{Config, Tab};
use crate::mandrill::{self, Auth, Message, Tag, Template, Webhook};
use anyhow::Result;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Instant;

/// Job sent from the TUI event loop to the background loader thread.
/// Every tab refresh (`refresh_active`) used to fire the 30s-timeout
/// `mandrill::*` call from inside the crossterm draw path —
/// auto-refresh ticks every `refresh_interval_secs` would freeze the
/// loop until the API returned. Hot path is now async.
#[derive(Debug)]
enum LoadJob {
    /// Refresh items for a specific tab. Carries the tab_idx so a
    /// late result for a now-inactive tab still lands in the right
    /// `ItemsTab` slot.
    RefreshTab {
        tab_idx: usize,
        kind: String,
        lookback_days: u64,
        auth: Auth,
    },
}

#[derive(Debug)]
enum LoadResult {
    TabItems {
        tab_idx: usize,
        kind: String,
        items: Vec<Item>,
        truncated: bool,
    },
    TabFailed {
        tab_idx: usize,
        error: String,
    },
}

fn spawn_loader(job_rx: Receiver<LoadJob>, res_tx: Sender<LoadResult>) {
    thread::Builder::new()
        .name("mnml-msg-mandrill-loader".into())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                match job {
                    LoadJob::RefreshTab {
                        tab_idx,
                        kind,
                        lookback_days,
                        auth,
                    } => {
                        let result: Result<(Vec<Item>, bool)> = match kind.as_str() {
                            "messages" => {
                                mandrill::search_messages(&auth, lookback_days).map(|msgs| {
                                    let truncated = msgs.len() >= mandrill::LIST_CAP;
                                    let items = msgs.into_iter().map(Item::Message).collect();
                                    (items, truncated)
                                })
                            }
                            "templates" => mandrill::list_templates(&auth).map(|ts| {
                                let truncated = ts.len() >= mandrill::LIST_CAP;
                                let items = ts.into_iter().map(Item::Template).collect();
                                (items, truncated)
                            }),
                            "tags" => mandrill::list_tags(&auth).map(|ts| {
                                let truncated = ts.len() >= mandrill::LIST_CAP;
                                let items = ts.into_iter().map(Item::Tag).collect();
                                (items, truncated)
                            }),
                            "webhooks" => mandrill::list_webhooks(&auth).map(|hs| {
                                let truncated = hs.len() >= mandrill::LIST_CAP;
                                let items = hs.into_iter().map(Item::Webhook).collect();
                                (items, truncated)
                            }),
                            _ => unreachable!("validated in TabSpec::resolve"),
                        };
                        match result {
                            Ok((items, truncated)) => {
                                let _ = res_tx.send(LoadResult::TabItems {
                                    tab_idx,
                                    kind,
                                    items,
                                    truncated,
                                });
                            }
                            Err(e) => {
                                let _ = res_tx.send(LoadResult::TabFailed {
                                    tab_idx,
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        })
        .expect("spawn loader thread");
}

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "messages" | "templates" | "tags" | "webhooks" => Ok(Self {
                kind: t.kind.clone(),
            }),
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
// Templates carry several optional String fields making the variant ~400 bytes;
// the v0.1 lists cap at 500 items, so the size cost is fine and boxing would
// force `.0` derefs everywhere a Template is matched.
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Message(Message),
    Template(Template),
    Tag(Tag),
    Webhook(Webhook),
}

impl Item {
    pub fn primary_label(&self) -> String {
        match self {
            Item::Message(m) => m.short_subject().to_string(),
            Item::Template(t) => {
                if t.name.is_empty() {
                    t.slug.clone()
                } else {
                    t.name.clone()
                }
            }
            Item::Tag(t) => t.tag.clone(),
            Item::Webhook(w) => w.url.clone(),
        }
    }

    pub fn secondary_label(&self) -> String {
        match self {
            Item::Message(m) => {
                let ts = short_time(m.ts);
                let to = if m.email.is_empty() {
                    "—"
                } else {
                    m.email.as_str()
                };
                format!("{ts}  {to}  · {}", m.state)
            }
            Item::Template(t) => {
                let subj = t.display_subject();
                let subj_disp = if subj.is_empty() {
                    "(no subject)"
                } else {
                    subj
                };
                format!("[{}] {}", t.publish_state(), subj_disp)
            }
            Item::Tag(t) => {
                let br_pct = t.bounce_rate() * 100.0;
                format!("{} sent · {:.2}% bounce", t.sent, br_pct)
            }
            Item::Webhook(w) => {
                let evs = if w.events.is_empty() {
                    "(no events)".to_string()
                } else {
                    w.events.join(",")
                };
                format!("{} · key {}", evs, w.auth_key_trailing())
            }
        }
    }
}

/// Format a Unix-epoch-seconds timestamp as `HH:MM:SS UTC` (best-effort).
fn short_time(ts: i64) -> String {
    if ts <= 0 {
        return "—".to_string();
    }
    use chrono::TimeZone;
    match chrono::Utc.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%m-%d %H:%M").to_string(),
        None => ts.to_string(),
    }
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
    /// Set when the source returned more than `LIST_CAP` items.
    pub truncated: bool,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
            truncated: false,
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
    pub auth: Auth,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    loader_tx: Sender<LoadJob>,
    loader_rx: Receiver<LoadResult>,
}

impl App {
    pub fn new(cfg: Config, auth: Auth) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t)?;
            tabs.push(TabState {
                name: t.name.clone(),
                data: ItemsTab::empty(),
                spec,
            });
        }
        let (job_tx, job_rx) = mpsc::channel();
        let (res_tx, res_rx) = mpsc::channel();
        spawn_loader(job_rx, res_tx);
        let mut app = App {
            cfg,
            auth,
            tabs,
            active_tab: 0,
            status: String::new(),
            loader_tx: job_tx,
            loader_rx: res_rx,
        };
        app.refresh_active();
        Ok(app)
    }

    /// Drain the loader-result channel. Call from `tick` BEFORE
    /// rendering each frame. Late results for now-inactive tabs land
    /// in the right `ItemsTab` slot via `tab_idx`.
    pub fn drain_load_results(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.loader_rx.try_recv() {
                Ok(LoadResult::TabItems {
                    tab_idx,
                    kind,
                    items,
                    truncated,
                }) => {
                    let Some(t) = self.tabs.get_mut(tab_idx) else {
                        continue;
                    };
                    let count = items.len();
                    t.data.loading = false;
                    t.data.items = items;
                    t.data.selected = t.data.selected.min(count.saturating_sub(1));
                    t.data.last_loaded = Some(Instant::now());
                    t.data.last_error = None;
                    t.data.truncated = truncated;
                    if tab_idx == self.active_tab {
                        let kind_label = match kind.as_str() {
                            "messages" => "messages",
                            "templates" => "templates",
                            "tags" => "tags",
                            "webhooks" => "webhooks",
                            _ => "items",
                        };
                        let extra = if truncated { " (capped)" } else { "" };
                        let name = t.name.clone();
                        self.status = format!("{name}: {count} {kind_label}{extra}");
                    }
                    changed = true;
                }
                Ok(LoadResult::TabFailed { tab_idx, error }) => {
                    let Some(t) = self.tabs.get_mut(tab_idx) else {
                        continue;
                    };
                    t.data.loading = false;
                    t.data.last_error = Some(error.clone());
                    if tab_idx == self.active_tab {
                        self.status = format!("error: {error}");
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.status = "loader thread died".to_string();
                    break;
                }
            }
        }
        changed
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

    /// Send a refresh job for the active tab to the loader thread.
    /// Non-blocking — old API was 30s+ on a slow network. Coalesces:
    /// if `loading` is already set we skip the send.
    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        if self.tabs[idx].data.loading {
            return;
        }
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;
        if let Err(e) = self.loader_tx.send(LoadJob::RefreshTab {
            tab_idx: idx,
            kind: spec.kind,
            lookback_days: self.cfg.messages_lookback_days,
            auth: self.auth.clone(),
        }) {
            self.tabs[idx].data.loading = false;
            self.status = format!("loader send failed: {e}");
        }
    }

    /// Tick — runs each frame. Drains loader results FIRST so any
    /// in-flight refresh shows up immediately, then triggers auto-
    /// refresh on stale tabs.
    pub fn tick(&mut self) -> bool {
        let loader_changed = self.drain_load_results();
        let idx = self.active_tab;
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return loader_changed;
        }
        let stale = match self.tabs[idx].data.last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].data.loading {
            self.refresh_active();
            true
        } else {
            loader_changed
        }
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    /// `o` / `Enter` — open the focused item in the Mandrill web UI.
    pub fn open_console(&mut self) {
        let url = match self.focused_item() {
            Some(Item::Message(m)) => mandrill::message_url(&m.id),
            Some(Item::Template(t)) => mandrill::template_url(&t.slug),
            Some(Item::Tag(t)) => mandrill::tag_url(&t.tag),
            Some(Item::Webhook(_)) => mandrill::webhooks_url().to_string(),
            None => {
                self.status = "no item under cursor".into();
                return;
            }
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` — yank the focused item's ID / name / URL.
    pub fn yank(&mut self) {
        let payload = match self.focused_item() {
            Some(Item::Message(m)) => m.id.clone(),
            Some(Item::Template(t)) => t.slug.clone(),
            Some(Item::Tag(t)) => t.tag.clone(),
            Some(Item::Webhook(w)) => w.url.clone(),
            None => {
                self.status = "no item under cursor".into();
                return;
            }
        };
        if payload.is_empty() {
            self.status = "nothing to copy".into();
            return;
        }
        let len = payload.chars().count();
        match crate::clipboard::copy(&payload) {
            Ok(()) => self.status = format!("copied ({len} chars)"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `L` — for a focused Message, dump the SMTP event log into a
    /// scratch text file under `$TMPDIR` and open it in `$PAGER`
    /// (default `less`). Best-effort; toasts on failure.
    pub fn event_log(&mut self) {
        let id = match self.focused_item() {
            Some(Item::Message(m)) => m.id.clone(),
            _ => {
                self.status = "L event log only available on messages".into();
                return;
            }
        };
        let detail = match mandrill::message_info(&self.auth, &id) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("message_info failed: {e}");
                return;
            }
        };
        let mut buf = String::new();
        buf.push_str(&format!("Mandrill message {id}\n"));
        buf.push_str(&format!("subject: {}\n", detail.subject));
        buf.push_str(&format!("to:      {}\n", detail.email));
        buf.push_str(&format!("from:    {}\n", detail.sender));
        buf.push_str(&format!("state:   {}\n", detail.state));
        buf.push_str(&format!(
            "ts:      {} ({})\n",
            detail.ts,
            short_time(detail.ts)
        ));
        if !detail.tags.is_empty() {
            buf.push_str(&format!("tags:    {}\n", detail.tags.join(", ")));
        }
        buf.push_str(&format!(
            "opens:   {} · clicks: {}\n",
            detail.opens, detail.clicks
        ));
        buf.push_str("\n── SMTP events ──\n");
        if detail.smtp_events.is_empty() {
            buf.push_str("(none)\n");
        } else {
            for ev in &detail.smtp_events {
                buf.push_str(&format!(
                    "  {} [{}] {}\n",
                    short_time(ev.ts),
                    ev.r#type,
                    ev.diag
                ));
            }
        }
        buf.push_str("\n── opens ──\n");
        if detail.opens_detail.is_empty() {
            buf.push_str("(none)\n");
        } else {
            for ev in &detail.opens_detail {
                buf.push_str(&format!("  {} {} {}\n", short_time(ev.ts), ev.ip, ev.ua));
            }
        }
        buf.push_str("\n── clicks ──\n");
        if detail.clicks_detail.is_empty() {
            buf.push_str("(none)\n");
        } else {
            for ev in &detail.clicks_detail {
                buf.push_str(&format!(
                    "  {} {} {} {}\n",
                    short_time(ev.ts),
                    ev.ip,
                    ev.url,
                    ev.ua
                ));
            }
        }

        let tmpdir = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let path = tmpdir.join(format!("mnml-msg-mandrill-{id}.txt"));
        if let Err(e) = std::fs::write(&path, &buf) {
            self.status = format!("write {} failed: {e}", path.display());
            return;
        }
        let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".into());
        match std::process::Command::new(&pager).arg(&path).status() {
            Ok(_) => self.status = format!("event log: {}", path.display()),
            Err(e) => {
                self.status = format!("spawn {pager} failed: {e} (log at {})", path.display())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolves_messages() {
        let t = Tab {
            name: "x".into(),
            kind: "messages".into(),
        };
        let spec = TabSpec::resolve(&t).unwrap();
        assert_eq!(spec.kind, "messages");
    }

    #[test]
    fn tab_spec_rejects_unknown_kind() {
        let t = Tab {
            name: "bad".into(),
            kind: "bogus".into(),
        };
        assert!(TabSpec::resolve(&t).is_err());
    }

    #[test]
    fn short_time_renders_known_epoch() {
        // 2026-06-07T12:34:56Z = 1780749296
        let s = short_time(1780749296);
        // The exact string depends on the chrono format, so just
        // check that we got something day/month/time-shaped (no `—`).
        assert!(!s.is_empty());
        assert_ne!(s, "—");
        assert!(s.contains(':'));
    }
}
