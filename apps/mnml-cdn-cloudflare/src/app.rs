//! App state — per-tab item lists + a selection cursor. Items are
//! a 5-variant enum because each tab kind has a distinct shape.

use crate::cloudflare::{self, Auth, DnsRecord, PagesProject, SecurityEvent, WorkerScript, Zone};
use crate::config::{Config, Tab};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    /// `dns` and `security_events`: the zone ID. Others: None.
    pub zone_id: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "zones" | "workers" | "pages" => Ok(Self {
                kind: t.kind.clone(),
                zone_id: None,
            }),
            "dns" | "security_events" => {
                let z = t.zone_id.clone().unwrap_or_default();
                if z.trim().is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"{}\" requires `zone_id`", t.name, t.kind);
                }
                Ok(Self {
                    kind: t.kind.clone(),
                    zone_id: Some(z),
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Zone(Zone),
    Dns(DnsRecord),
    Worker(WorkerScript),
    Pages(PagesProject),
    Security(SecurityEvent),
}

impl Item {
    pub fn primary_label(&self) -> String {
        match self {
            Item::Zone(z) => {
                if z.name.is_empty() {
                    "(unnamed)".into()
                } else {
                    z.name.clone()
                }
            }
            Item::Dns(d) => {
                if d.name.is_empty() {
                    "(unnamed)".into()
                } else {
                    d.name.clone()
                }
            }
            Item::Worker(w) => w.id.clone(),
            Item::Pages(p) => p.name.clone(),
            Item::Security(e) => e.action.clone(),
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Zone(z) => {
                let paused = if z.paused { " · paused" } else { "" };
                format!("{} · {}{}", z.status, z.plan_name(), paused)
            }
            Item::Dns(d) => {
                let content = if d.content.chars().count() > 40 {
                    let mut s: String = d.content.chars().take(39).collect();
                    s.push('…');
                    s
                } else {
                    d.content.clone()
                };
                let proxied = if d.proxied { " · proxied" } else { "" };
                let ttl = if d.ttl == 1 {
                    "auto".to_string()
                } else {
                    format!("{}s", d.ttl)
                };
                format!("{} · {} · {}{}", d.record_type, content, ttl, proxied)
            }
            Item::Worker(w) => {
                let modified = w.modified_on.as_deref().unwrap_or("—");
                format!("modified {}", short_timestamp(modified))
            }
            Item::Pages(p) => {
                let branch = p.production_branch.as_deref().unwrap_or("—");
                format!(
                    "{} · {} · {}",
                    p.primary_domain(),
                    branch,
                    p.last_deploy_status()
                )
            }
            Item::Security(e) => {
                let ts = e
                    .occurred_at
                    .as_deref()
                    .map(short_timestamp)
                    .unwrap_or_else(|| "—".into());
                let ip = e.client_ip.as_deref().unwrap_or("—");
                let rule = e.rule_id.as_deref().unwrap_or("—");
                format!("{ts} · {ip} · {} · {rule}", e.source)
            }
        }
    }
}

fn short_timestamp(ts: &str) -> String {
    // `2026-01-01T12:34:56Z` → `12:34:56`. Best-effort.
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

/// Two-state machine for the `X` (purge cache) destructive action.
/// `None` outside of a confirmation; `Some(zone_id)` while awaiting
/// `y` / `n`.
#[derive(Debug, Clone)]
pub enum ConfirmState {
    None,
    PurgeCache { zone_id: String, zone_name: String },
}

pub struct App {
    pub cfg: Config,
    pub auth: Auth,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    pub confirm: ConfirmState,
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
            confirm: ConfirmState::None,
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
            self.confirm = ConfirmState::None;
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
            "zones" => cloudflare::list_zones(&self.auth).map(|zones| {
                let truncated = zones.len() >= cloudflare::LIST_CAP;
                let items = zones.into_iter().map(Item::Zone).collect();
                (items, truncated)
            }),
            "dns" => {
                let zid = spec.zone_id.as_deref().unwrap_or("");
                cloudflare::list_dns_records(&self.auth, zid).map(|recs| {
                    let truncated = recs.len() >= cloudflare::LIST_CAP;
                    let items = recs.into_iter().map(Item::Dns).collect();
                    (items, truncated)
                })
            }
            "workers" => match self.auth.account_id.as_deref() {
                Some(acc) => cloudflare::list_workers(&self.auth, acc).map(|ws| {
                    let truncated = ws.len() >= cloudflare::LIST_CAP;
                    let items = ws.into_iter().map(Item::Worker).collect();
                    (items, truncated)
                }),
                None => Err(anyhow::anyhow!(
                    "workers tab needs CLOUDFLARE_ACCOUNT_ID — export it from dash.cloudflare.com sidebar"
                )),
            },
            "pages" => match self.auth.account_id.as_deref() {
                Some(acc) => cloudflare::list_pages_projects(&self.auth, acc).map(|ps| {
                    let truncated = ps.len() >= cloudflare::LIST_CAP;
                    let items = ps.into_iter().map(Item::Pages).collect();
                    (items, truncated)
                }),
                None => Err(anyhow::anyhow!(
                    "pages tab needs CLOUDFLARE_ACCOUNT_ID — export it from dash.cloudflare.com sidebar"
                )),
            },
            "security_events" => {
                let zid = spec.zone_id.as_deref().unwrap_or("");
                cloudflare::list_security_events(&self.auth, zid).map(|evs| {
                    let items = evs.into_iter().map(Item::Security).collect::<Vec<_>>();
                    (items, false)
                })
            }
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
                    "zones" => "zones",
                    "dns" => "DNS records",
                    "workers" => "workers",
                    "pages" => "pages projects",
                    "security_events" => "security events",
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

    pub fn tick(&mut self) -> bool {
        let idx = self.active_tab;
        let interval = self.cfg.refresh_interval_secs;
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

    /// `o` / `Enter` — open the focused item in the Cloudflare dash.
    pub fn open_dashboard(&mut self) {
        let acc = self.auth.account_id.as_deref();
        let url = match self.focused_item() {
            Some(Item::Zone(z)) => cloudflare::zone_dashboard_url(acc, &z.name),
            Some(Item::Dns(d)) => cloudflare::dns_dashboard_url(acc, &d.zone_name),
            Some(Item::Worker(w)) => match acc {
                Some(a) => cloudflare::worker_dashboard_url(a, &w.id),
                None => {
                    self.status = "no CLOUDFLARE_ACCOUNT_ID set — can't build worker URL".into();
                    return;
                }
            },
            Some(Item::Pages(p)) => match acc {
                Some(a) => cloudflare::pages_dashboard_url(a, &p.name),
                None => {
                    self.status = "no CLOUDFLARE_ACCOUNT_ID set — can't build pages URL".into();
                    return;
                }
            },
            Some(Item::Security(_)) => {
                // Security-events tab doesn't carry zone name on each event —
                // fall back to the tab's zone via the spec.
                let zone_id = self.active().spec.zone_id.clone().unwrap_or_default();
                cloudflare::security_dashboard_url(acc, &zone_id)
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

    /// `y` — yank the focused item's ID.
    pub fn yank(&mut self) {
        let payload = match self.focused_item() {
            Some(Item::Zone(z)) => z.id.clone(),
            Some(Item::Dns(d)) => d.id.clone(),
            Some(Item::Worker(w)) => w.id.clone(),
            Some(Item::Pages(p)) => p.id.clone(),
            Some(Item::Security(e)) => e.ray_id.clone().unwrap_or_default(),
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
            Ok(()) => self.status = format!("copied ID ({len} chars)"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `X` — request cache purge for the focused zone (zones tab
    /// only). Sets `ConfirmState::PurgeCache`; the real call happens
    /// in `confirm_yes`.
    pub fn request_purge(&mut self) {
        let (zone_id, zone_name) = match self.focused_item() {
            Some(Item::Zone(z)) => (z.id.clone(), z.name.clone()),
            _ => {
                self.status = "X only available on zones".into();
                return;
            }
        };
        self.status = format!("purge ALL cache for {zone_name} ?  [y/n]");
        self.confirm = ConfirmState::PurgeCache { zone_id, zone_name };
    }

    /// `y` while a confirm is pending — execute it.
    pub fn confirm_yes(&mut self) {
        let confirm = std::mem::replace(&mut self.confirm, ConfirmState::None);
        match confirm {
            ConfirmState::PurgeCache { zone_id, zone_name } => {
                match cloudflare::purge_cache(&self.auth, &zone_id) {
                    Ok(()) => self.status = format!("purged cache for {zone_name}"),
                    Err(e) => self.status = format!("purge failed: {e}"),
                }
            }
            ConfirmState::None => {}
        }
    }

    /// `n` / `Esc` while a confirm is pending — drop it.
    pub fn confirm_no(&mut self) {
        if !matches!(self.confirm, ConfirmState::None) {
            self.confirm = ConfirmState::None;
            self.status = "cancelled".into();
        }
    }

    /// `D` — toggle development mode (zones tab only). Re-fetches
    /// the zone to refresh the detail panel.
    pub fn toggle_dev_mode(&mut self) {
        let (zone_id, zone_name, currently_on) = match self.focused_item() {
            Some(Item::Zone(z)) => (
                z.id.clone(),
                z.name.clone(),
                z.development_mode.unwrap_or(0) > 0,
            ),
            _ => {
                self.status = "D only available on zones".into();
                return;
            }
        };
        // dev_mode > 0 means it's on; flip it.
        let target = !currently_on;
        match cloudflare::set_dev_mode(&self.auth, &zone_id, target) {
            Ok(new_val) => {
                self.status = format!("dev mode {new_val} for {zone_name}");
                // Re-fetch the zone so the detail panel updates.
                if let Ok(updated) = cloudflare::get_zone(&self.auth, &zone_id) {
                    let idx = self.active_tab;
                    let sel = self.tabs[idx].data.selected;
                    if let Some(Item::Zone(z)) = self.tabs[idx].data.items.get_mut(sel) {
                        *z = updated;
                    }
                }
            }
            Err(e) => self.status = format!("dev_mode toggle failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolves_dns_with_zone_id() {
        let t = Tab {
            name: "x".into(),
            kind: "dns".into(),
            zone_id: Some("abc".into()),
        };
        let spec = TabSpec::resolve(&t).unwrap();
        assert_eq!(spec.kind, "dns");
        assert_eq!(spec.zone_id.as_deref(), Some("abc"));
    }

    #[test]
    fn tab_spec_rejects_dns_without_zone_id() {
        let t = Tab {
            name: "bad".into(),
            kind: "dns".into(),
            zone_id: None,
        };
        assert!(TabSpec::resolve(&t).is_err());
    }

    #[test]
    fn short_timestamp_extracts_hms() {
        assert_eq!(short_timestamp("2026-01-01T12:34:56Z"), "12:34:56");
    }

    #[test]
    fn confirm_state_machine_purge() {
        // Stand up a minimal app — no real network calls because we
        // never call refresh_active in this test.
        let auth = Auth {
            token: "x".into(),
            account_id: None,
        };
        let cfg = Config {
            refresh_interval_secs: 0,
            tabs: vec![Tab {
                name: "Zones".into(),
                kind: "zones".into(),
                zone_id: None,
            }],
        };
        let spec = TabSpec::resolve(&cfg.tabs[0]).unwrap();
        let mut app = App {
            cfg,
            auth,
            tabs: vec![TabState {
                name: "Zones".into(),
                spec,
                data: ItemsTab::empty(),
            }],
            active_tab: 0,
            status: String::new(),
            confirm: ConfirmState::None,
        };
        // No item focused — request_purge should bail.
        app.request_purge();
        assert!(matches!(app.confirm, ConfirmState::None));

        // Push a fake zone, focus it, request purge.
        app.tabs[0].data.items.push(Item::Zone(Zone {
            id: "zid1".into(),
            name: "example.com".into(),
            status: "active".into(),
            paused: false,
            plan: None,
            name_servers: vec![],
            original_name_servers: vec![],
            development_mode: Some(0),
            modified_on: None,
            account: None,
        }));
        app.request_purge();
        assert!(matches!(app.confirm, ConfirmState::PurgeCache { .. }));
        assert!(app.status.contains("[y/n]"));

        // Cancel with `n`.
        app.confirm_no();
        assert!(matches!(app.confirm, ConfirmState::None));
        assert!(app.status.contains("cancelled"));
    }
}
