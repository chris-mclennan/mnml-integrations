//! App state — per-tab item lists + a selection cursor. Items are
//! a 4-variant enum because each tab kind has a distinct shape.

use crate::config::{Config, Tab};
use crate::datadog::{self, Auth, Dashboard, Incident, LogEvent, Monitor};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    /// `monitors`: tag scope; `dashboards`: title prefix; `logs`: the
    /// log query; `incidents`: ignored.
    pub query: Option<String>,
    /// `logs`-only: time window (e.g. `now-15m`).
    pub from: Option<String>,
    /// `logs`-only: poll interval. Defaults to 5s when unset.
    pub tail_interval_secs: Option<u64>,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "monitors" | "dashboards" | "incidents" => Ok(Self {
                kind: t.kind.clone(),
                query: t.query.clone(),
                from: t.from.clone(),
                tail_interval_secs: t.tail_interval_secs,
            }),
            "logs" => {
                let q = t.query.clone().unwrap_or_default();
                if q.trim().is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"logs\" requires `query`", t.name);
                }
                Ok(Self {
                    kind: "logs".into(),
                    query: Some(q),
                    from: t.from.clone(),
                    tail_interval_secs: t.tail_interval_secs,
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Monitor(Monitor),
    Dashboard(Dashboard),
    Log(LogEvent),
    Incident(Incident),
}

impl Item {
    pub fn primary_label(&self) -> String {
        match self {
            Item::Monitor(m) => m.short_name().to_string(),
            Item::Dashboard(d) => {
                if d.title.is_empty() {
                    "(untitled)".to_string()
                } else {
                    d.title.clone()
                }
            }
            Item::Log(l) => l.attributes.service.clone().unwrap_or_else(|| "—".into()),
            Item::Incident(i) => {
                if i.attributes.title.is_empty() {
                    "(no title)".to_string()
                } else {
                    i.attributes.title.clone()
                }
            }
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Monitor(m) => {
                let state = if m.overall_state.is_empty() {
                    "—".to_string()
                } else {
                    m.overall_state.clone()
                };
                let kind_short = m.monitor_type.replace(" alert", "");
                format!("{state} · {kind_short}")
            }
            Item::Dashboard(d) => d.author_handle.clone().unwrap_or_else(|| "—".into()),
            Item::Log(l) => {
                let ts = l
                    .attributes
                    .timestamp
                    .as_deref()
                    .map(short_timestamp)
                    .unwrap_or_else(|| "—".into());
                let status = l.attributes.status.as_deref().unwrap_or("—");
                let msg = l
                    .attributes
                    .message
                    .as_deref()
                    .map(|m| {
                        let m = m.lines().next().unwrap_or(m);
                        if m.chars().count() > 80 {
                            let mut s: String = m.chars().take(79).collect();
                            s.push('…');
                            s
                        } else {
                            m.to_string()
                        }
                    })
                    .unwrap_or_default();
                format!("{ts} [{status}] {msg}")
            }
            Item::Incident(i) => {
                let sev = i.attributes.severity.as_deref().unwrap_or("—");
                let state = i.attributes.state.as_deref().unwrap_or("—");
                format!("{sev} · {state}")
            }
        }
    }
}

fn short_timestamp(ts: &str) -> String {
    // `2026-01-01T12:34:56.789Z` → `12:34:56`. Best-effort.
    if let Some(after_t) = ts.split_once('T') {
        let time = after_t.1;
        return time.chars().take(8).collect();
    }
    ts.to_string()
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
        let mut app = App {
            cfg,
            auth,
            tabs,
            active_tab: 0,
            status: String::new(),
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

        let result: Result<(Vec<Item>, bool)> = match spec.kind.as_str() {
            "monitors" => {
                let tag = spec.query.as_deref();
                datadog::list_monitors(&self.auth, tag).map(|mons| {
                    let truncated = mons.len() >= datadog::LIST_CAP;
                    let items = mons.into_iter().map(Item::Monitor).collect();
                    (items, truncated)
                })
            }
            "dashboards" => {
                let prefix = spec.query.as_deref();
                datadog::list_dashboards(&self.auth, prefix).map(|dashes| {
                    let truncated = dashes.len() >= datadog::LIST_CAP;
                    let items = dashes.into_iter().map(Item::Dashboard).collect();
                    (items, truncated)
                })
            }
            "logs" => {
                let query = spec.query.as_deref().unwrap_or("");
                let from = spec.from.as_deref().unwrap_or("now-15m");
                datadog::search_logs(&self.auth, query, from).map(|logs| {
                    let items = logs.into_iter().map(Item::Log).collect::<Vec<_>>();
                    (items, false)
                })
            }
            "incidents" => datadog::list_active_incidents(&self.auth).map(|incs| {
                let items = incs.into_iter().map(Item::Incident).collect::<Vec<_>>();
                (items, false)
            }),
            _ => unreachable!("validated in TabSpec::resolve"),
        };

        let t = &mut self.tabs[idx];
        t.data.loading = false;
        match result {
            Ok((items, truncated)) => {
                let count = items.len();
                t.data.items = items;
                t.data.selected = t.data.selected.min(count.saturating_sub(1));
                t.data.last_loaded = Some(Instant::now());
                t.data.last_error = None;
                t.data.truncated = truncated;
                let kind_label = match spec.kind.as_str() {
                    "monitors" => "monitors",
                    "dashboards" => "dashboards",
                    "logs" => "log events",
                    "incidents" => "incidents",
                    _ => "items",
                };
                let extra = if truncated { " (capped)" } else { "" };
                self.status = format!("{name}: {count} {kind_label}{extra}");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Tick — runs each frame. Two refresh paths:
    ///   * `logs` tabs live-tail on their own `tail_interval_secs`
    ///     (defaults to 5s) when they're the *focused* tab.
    ///   * Other tabs honor the global `refresh_interval_secs`.
    pub fn tick(&mut self) -> bool {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        let interval = if kind == "logs" {
            self.tabs[idx].spec.tail_interval_secs.unwrap_or(5)
        } else {
            self.cfg.refresh_interval_secs
        };
        if interval == 0 {
            return false;
        }
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

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    /// `o` / `Enter` — open the focused item in the Datadog web UI.
    pub fn open_console(&mut self) {
        let url = match self.focused_item() {
            Some(Item::Monitor(m)) => datadog::monitor_url(&self.auth, m.id),
            Some(Item::Dashboard(d)) => datadog::dashboard_url(&self.auth, d),
            Some(Item::Incident(i)) => datadog::incident_url(&self.auth, i),
            Some(Item::Log(_)) => {
                // Logs don't have a per-event web URL — fall back to
                // the logs explorer pre-scoped to the tab's query.
                let q = self.active().spec.query.clone().unwrap_or_default();
                datadog::logs_url(&self.auth, &q)
            }
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

    /// `y` — yank a useful string for the focused item.
    ///   * Monitor / Dashboard / Incident: the web URL
    ///   * Log: the message body (one line)
    pub fn yank(&mut self) {
        let payload = match self.focused_item() {
            Some(Item::Monitor(m)) => datadog::monitor_url(&self.auth, m.id),
            Some(Item::Dashboard(d)) => datadog::dashboard_url(&self.auth, d),
            Some(Item::Incident(i)) => datadog::incident_url(&self.auth, i),
            Some(Item::Log(l)) => l.attributes.message.clone().unwrap_or_default(),
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
            Ok(()) => self.status = format!("copied URL ({len} chars)"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `L` — cross-sibling handoff: when focused on a monitor whose
    /// query references an AWS log group, spawn
    /// `mnml-aws-cloudwatch-logs --log-group <group>`. Best-effort —
    /// the detection is heuristic.
    pub fn handoff_cloudwatch(&mut self) {
        let Some(Item::Monitor(m)) = self.focused_item() else {
            self.status = "L jump only available on monitors".into();
            return;
        };
        let Some(group) = datadog::extract_log_group(&m.query) else {
            self.status =
                "no AWS log group detected in monitor query (look for `aws_log_group:` or `/aws/...`)".into();
            return;
        };
        match std::process::Command::new("mnml-aws-cloudwatch-logs")
            .arg("--log-group")
            .arg(&group)
            .spawn()
        {
            Ok(_) => self.status = format!("launched mnml-aws-cloudwatch-logs for {group}"),
            Err(e) => self.status = format!("spawn mnml-aws-cloudwatch-logs failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolves_logs_with_query() {
        let t = Tab {
            name: "x".into(),
            kind: "logs".into(),
            query: Some("status:error".into()),
            from: None,
            tail_interval_secs: None,
        };
        let spec = TabSpec::resolve(&t).unwrap();
        assert_eq!(spec.kind, "logs");
        assert_eq!(spec.query.as_deref(), Some("status:error"));
    }

    #[test]
    fn tab_spec_rejects_logs_without_query() {
        let t = Tab {
            name: "bad".into(),
            kind: "logs".into(),
            query: None,
            from: None,
            tail_interval_secs: None,
        };
        assert!(TabSpec::resolve(&t).is_err());
    }

    #[test]
    fn short_timestamp_extracts_hms() {
        assert_eq!(short_timestamp("2026-01-01T12:34:56.789Z"), "12:34:56");
    }
}
