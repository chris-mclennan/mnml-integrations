//! App state — per-tab loaded data + selection.

use crate::amplify::{self, AmplifyApp, AmplifyBranch, AmplifyEvent, AmplifyJob};
use crate::config::{Config, Tab};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

/// A single visible row on the unified list. The list is a flat
/// projection of `AppsTab.items` where each app header is
/// followed inline by its (filtered) branches when the header is
/// expanded.
#[derive(Debug, Clone)]
pub enum VisibleRow {
    AppHeader {
        items_index: usize,
        expanded: bool,
    },
    Branch {
        app_id: String,
        branch_name: String,
    },
    /// Emitted after an expanded `Branch` row when jobs are
    /// available — one per job, up to 3. Selectable so arrow
    /// navigation lands on a specific deployment; Enter/click
    /// drills straight into that job's logs.
    Deployment {
        app_id: String,
        branch_name: String,
        job_id: String,
    },
}

/// Which branches always show under an expanded app.
pub fn is_primary_branch(name: &str) -> bool {
    if name.starts_with("release/") {
        return true;
    }
    matches!(
        name,
        "main" | "master" | "develop" | "staging" | "production"
    )
}

/// Order among primary branches: main → develop → staging →
/// release/* → other-primary.
pub fn primary_sort_key(name: &str) -> u8 {
    match name {
        "main" | "master" => 0,
        "develop" => 1,
        "staging" => 2,
        n if n.starts_with("release/") => 3,
        _ => 4,
    }
}

/// Timestamp key for "which auxiliary branch deployed most
/// recently". Later timestamp = larger string. Missing =
/// empty (sorts last).
pub fn last_deploy_ts_key(a: &AppsTab, app_id: &str, branch: &str) -> String {
    a.jobs_by_key
        .get(&(app_id.to_string(), branch.to_string()))
        .and_then(|js| js.first())
        .and_then(|j| j.end_time.clone().or_else(|| j.start_time.clone()))
        .unwrap_or_default()
}

/// Drop feature + release branches whose last activity is older
/// than this many days. Matches mnml-forge-bitbucket's rule.
/// User: "amplify is missing col headers ... im seeing old feat
/// branches from 2025, can we do same thing. also 2 weeks not
/// 45 days." Essential branches (main / master / develop /
/// staging / production) are never filtered — they're eternal
/// environments in AWS Amplify.
pub const STALE_AFTER_DAYS: i64 = 14;

/// Extract days-since-now from an ISO-8601 date (or the leading
/// `YYYY-MM-DD` prefix of any timestamp). None on parse failure —
/// the caller treats that as "unknown age, keep it" so we err on
/// showing rather than hiding. 2026-07-20.
pub fn days_since_iso(iso: &str) -> Option<i64> {
    let date_part = iso.split('T').next()?;
    let mut parts = date_part.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let today_jd = 2440588 + now / 86_400;
    Some(today_jd - jd_of(y, m, d))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Apps,
    App,
}

impl TabKind {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "apps" => Ok(Self::Apps),
            "app" => Ok(Self::App),
            other => anyhow::bail!("unknown tab kind: {other}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: TabKind,
    pub region: Option<String>,
    pub app_id: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let kind = TabKind::from_str(&t.kind)?;
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match kind {
            TabKind::Apps => Ok(Self {
                kind,
                region,
                app_id: None,
            }),
            TabKind::App => {
                let app_id = t
                    .app_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("`app_id` required for kind `app`"))?;
                Ok(Self {
                    kind,
                    region,
                    app_id: Some(app_id),
                })
            }
        }
    }
}

pub enum TabData {
    Apps(AppsTab),
    App(AppTab),
}

pub struct AppsTab {
    /// Raw apps as returned by the AWS CLI (unfiltered). The UI
    /// applies the hidden-apps filter at render time, so `x` can
    /// hide/unhide without a refetch.
    pub items: Vec<AmplifyApp>,
    pub selected: usize,
    pub last_error: Option<String>,
    pub loading: bool,
    pub pending: Option<Receiver<AmplifyEvent>>,
    pub last_fetched: Option<std::time::Instant>,
    /// When true, the UI shows hidden apps dimmed instead of
    /// filtering them out entirely. Toggled by `H`; lets the user
    /// see what they've hidden + unhide with `x`.
    pub show_hidden: bool,
    /// app_ids the user has toggled open. Header rows for these
    /// apps get a ▼ chevron and are followed by their (filtered)
    /// branch rows inline.
    pub expanded: HashSet<String>,
    /// Branches per app, populated on first expand via
    /// `spawn_list_branches`. Missing = not fetched yet; empty vec
    /// = fetched and app has no branches.
    pub branches_by_app: HashMap<String, Vec<AmplifyBranch>>,
    /// Latest jobs per (app_id, branch). Populated after branches
    /// arrive by fanning out `spawn_list_jobs`.
    pub jobs_by_key: HashMap<(String, String), Vec<AmplifyJob>>,
    /// Last error from `spawn_list_jobs` per (app_id, branch).
    /// Surfaced in the branch-expand block so throttles / perm
    /// denials / whatever aren't invisible. Cleared on next
    /// successful jobs reply.
    pub jobs_error_by_key: HashMap<(String, String), String>,
    /// In-flight branch fetches keyed by app_id.
    pub pending_branches: HashMap<String, Receiver<AmplifyEvent>>,
    /// In-flight per-branch job fetches — each carries the app_id
    /// through so the Jobs event can be routed back to
    /// `jobs_by_key`.
    pub pending_jobs: Vec<(String, String, Receiver<AmplifyEvent>)>,
}

pub struct AppTab {
    pub branches: Vec<AmplifyBranch>,
    pub jobs_for_selected_branch: Vec<AmplifyJob>,
    /// Recent jobs per branch (latest 5) — powers the inline
    /// `<branch>  current  last: SUCCEED #123 (2h ago)` display.
    /// Populated when Branches arrive by fanning out one
    /// list_jobs per branch. Keyed by branch_name.
    pub jobs_by_branch: HashMap<String, Vec<AmplifyJob>>,
    /// Per-branch in-flight list-jobs channels for the fan-out
    /// initiated after Branches load. Drained into
    /// `jobs_by_branch` as responses land.
    pub pending_per_branch: Vec<Receiver<AmplifyEvent>>,
    pub selected: usize,
    pub last_error: Option<String>,
    pub loading: bool,
    pub pending: Option<Receiver<AmplifyEvent>>,
    pub pending_jobs: Option<Receiver<AmplifyEvent>>,
    pub last_fetched: Option<std::time::Instant>,
}

impl TabData {
    pub fn empty_for(kind: TabKind) -> Self {
        match kind {
            TabKind::Apps => Self::Apps(AppsTab {
                items: Vec::new(),
                selected: 0,
                last_error: None,
                loading: false,
                pending: None,
                last_fetched: None,
                show_hidden: false,
                expanded: HashSet::new(),
                branches_by_app: HashMap::new(),
                jobs_by_key: HashMap::new(),
                jobs_error_by_key: HashMap::new(),
                pending_branches: HashMap::new(),
                pending_jobs: Vec::new(),
            }),
            TabKind::App => Self::App(AppTab {
                branches: Vec::new(),
                jobs_for_selected_branch: Vec::new(),
                jobs_by_branch: HashMap::new(),
                pending_per_branch: Vec::new(),
                selected: 0,
                last_error: None,
                loading: false,
                pending: None,
                pending_jobs: None,
                last_fetched: None,
            }),
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: TabData,
}

/// Overlay state for the Enter → logs drill-in flow. Owns the
/// pending job-detail + per-step-log receivers plus the rendered
/// text as it streams in. Set to `Some` while the overlay is
/// visible; `None` back on the branches list.
pub struct LogsView {
    pub branch_name: String,
    pub job_id: Option<String>,
    pub commit_message: Option<String>,
    /// True while we're waiting on the initial `get-job` reply.
    pub loading_detail: bool,
    pub detail_rx: Option<Receiver<AmplifyEvent>>,
    /// One receiver per step we've fired a log fetch for. Drained
    /// each tick; entries removed as their logs land.
    pub log_rxs: Vec<Receiver<AmplifyEvent>>,
    /// (step_name, log_text_or_error) in step order. Populated as
    /// step-log fetches complete.
    pub steps: Vec<(String, String)>,
    pub error: Option<String>,
    /// Scroll offset (rows from the top).
    pub scroll: u16,
}

/// Deployment history overlay — mirrors the AWS Amplify console's
/// "Deployments" tab for one branch. Top: latest deployment
/// summary. Bottom: scrollable table of every past deployment.
/// Enter on a row drills into that job's logs via `LogsView`.
pub struct DeploymentHistoryView {
    pub app_id: String,
    pub app_name: String,
    pub branch_name: String,
    pub loading: bool,
    pub pending_rx: Option<Receiver<AmplifyEvent>>,
    pub jobs: Vec<AmplifyJob>,
    pub selected: usize,
    pub scroll: u16,
    pub error: Option<String>,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    pub logs_view: Option<LogsView>,
    pub deployment_history: Option<DeploymentHistoryView>,
    /// 2026-08-01 — branches with an inline "last deploy detail"
    /// block expanded below the row. Keyed by (app_id, branch_name)
    /// so the same branch name in two apps expands independently.
    /// Mirrors the merged-PR expand shipped in mnml-forge-bitbucket
    /// (`expanded_prs`). Not persisted — a session-scope toggle.
    pub expanded_branches: HashSet<(String, String)>,
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        // 2026-07-03 unified view: config's `[[tabs]]` array is
        // preserved on disk for backward compatibility but the
        // runtime is now a single "All apps" tab. Each app row
        // is expandable to reveal its branches. App-kind tabs
        // (drill-into-one-app) can still be authored in TOML —
        // treat their app_id as pre-expanded on startup so a
        // user who's been staring at a specific app gets it open
        // by default.
        let region = cfg.region.clone();
        let pre_expanded: HashSet<String> = cfg
            .tabs
            .iter()
            .filter(|t| t.kind == "app")
            .filter_map(|t| t.app_id.clone())
            .collect();
        let single = TabState {
            name: "All apps".to_string(),
            spec: TabSpec {
                kind: TabKind::Apps,
                region,
                app_id: None,
            },
            data: TabData::Apps(AppsTab {
                items: Vec::new(),
                selected: 0,
                last_error: None,
                loading: false,
                pending: None,
                last_fetched: None,
                show_hidden: false,
                expanded: pre_expanded,
                branches_by_app: HashMap::new(),
                jobs_by_key: HashMap::new(),
                jobs_error_by_key: HashMap::new(),
                pending_branches: HashMap::new(),
                pending_jobs: Vec::new(),
            }),
        };
        let mut app = App {
            cfg,
            tabs: vec![single],
            active_tab: 0,
            status: String::new(),
            logs_view: None,
            deployment_history: None,
            expanded_branches: HashSet::new(),
        };
        app.refresh_active();
        Ok(app)
    }

    /// Flatten the current apps + expansion state into the ordered
    /// list of rows the UI will render. Called each frame + used
    /// by cursor arithmetic. Cheap enough not to cache (N apps +
    /// (few) branches per expanded app).
    pub fn visible_rows(&self) -> Vec<VisibleRow> {
        let Some(TabState {
            data: TabData::Apps(a),
            ..
        }) = self.tabs.first()
        else {
            return Vec::new();
        };
        let hidden = &self.cfg.hidden_app_ids;
        let ordered = self.ordered_items_indices();
        let mut rows: Vec<VisibleRow> = Vec::new();
        for i in ordered {
            let Some(app) = a.items.get(i) else { continue };
            let is_hidden = hidden.iter().any(|h| h == &app.app_id);
            if is_hidden && !a.show_hidden {
                continue;
            }
            let expanded = a.expanded.contains(&app.app_id);
            rows.push(VisibleRow::AppHeader {
                items_index: i,
                expanded,
            });
            if expanded {
                for br in Self::filtered_branches_for(a, &app.app_id) {
                    rows.push(VisibleRow::Branch {
                        app_id: app.app_id.clone(),
                        branch_name: br.branch_name.clone(),
                    });
                    // If this branch is inline-expanded AND has cached
                    // jobs, emit selectable Deployment rows for the
                    // most-recent 3 so ↑↓+Enter can drill in.
                    // Loading / empty / error placeholders stay as
                    // an extra line on the Branch row itself.
                    let bkey = (app.app_id.clone(), br.branch_name.clone());
                    if self.expanded_branches.contains(&bkey) {
                        if let Some(js) = a.jobs_by_key.get(&bkey) {
                            if !js.is_empty() {
                                for j in js.iter().take(3) {
                                    rows.push(VisibleRow::Deployment {
                                        app_id: app.app_id.clone(),
                                        branch_name: br.branch_name.clone(),
                                        job_id: j.job_id.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        rows
    }

    /// Index-sequence into `a.items` that reflects the user's
    /// preferred order: `[cfg.app_order]` entries first (in that
    /// exact order), then any app whose id ISN'T in `app_order`
    /// in the natural AWS order.
    fn ordered_items_indices(&self) -> Vec<usize> {
        let Some(TabState {
            data: TabData::Apps(a),
            ..
        }) = self.tabs.first()
        else {
            return Vec::new();
        };
        let idx_by_id: std::collections::HashMap<&str, usize> = a
            .items
            .iter()
            .enumerate()
            .map(|(i, app)| (app.app_id.as_str(), i))
            .collect();
        let mut used = std::collections::HashSet::new();
        let mut out: Vec<usize> = Vec::with_capacity(a.items.len());
        for id in &self.cfg.app_order {
            if let Some(&i) = idx_by_id.get(id.as_str()) {
                out.push(i);
                used.insert(i);
            }
        }
        for (i, _) in a.items.iter().enumerate() {
            if !used.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// Alt-↑ — swap the focused app with the one above it in the
    /// user's ordering. Writes the new order back to config so
    /// it survives restart.
    pub fn move_app_up(&mut self) {
        self.move_app_in_order(-1);
    }

    /// Alt-↓ — same as above but downward.
    pub fn move_app_down(&mut self) {
        self.move_app_in_order(1);
    }

    fn move_app_in_order(&mut self, delta: isize) {
        let rows = self.visible_rows();
        let sel = self.selected_index();
        let Some(VisibleRow::AppHeader { items_index, .. }) = rows.get(sel).cloned() else {
            self.status = "reorder is only on an app header row".into();
            return;
        };
        let TabData::Apps(a) = &self.active().data else {
            return;
        };
        let Some(app) = a.items.get(items_index) else {
            return;
        };
        let target_id = app.app_id.clone();
        let target_name = app.name.clone();
        // Build the current ordered id list (visible in the UI).
        // We reorder within THIS list — hidden apps stay where
        // they were relative to each other.
        let hidden = self.cfg.hidden_app_ids.clone();
        let show_hidden = a.show_hidden;
        let ordered: Vec<String> = self
            .ordered_items_indices()
            .into_iter()
            .filter_map(|i| {
                let app = a.items.get(i)?;
                let is_hidden = hidden.iter().any(|h| h == &app.app_id);
                if is_hidden && !show_hidden {
                    return None;
                }
                Some(app.app_id.clone())
            })
            .collect();
        let Some(pos) = ordered.iter().position(|id| id == &target_id) else {
            return;
        };
        let new_pos = pos as isize + delta;
        if new_pos < 0 || new_pos >= ordered.len() as isize {
            return;
        }
        let new_pos = new_pos as usize;
        // Reorder within `ordered`, then serialize into app_order.
        let mut new_ordered = ordered.clone();
        new_ordered.swap(pos, new_pos);
        self.cfg.app_order = new_ordered;
        if let Err(e) = crate::config::save(&self.cfg) {
            self.status = format!("reordered {target_name} in memory (save failed: {e})");
        } else {
            self.status = format!(
                "moved {target_name} {}",
                if delta < 0 { "up" } else { "down" }
            );
        }
        // Follow the moved app so the cursor stays with it.
        // Recompute rows since ordering changed.
        let rows = self.visible_rows();
        let new_sel = rows.iter().position(|r| {
            if let VisibleRow::AppHeader { items_index: i, .. } = r {
                let TabData::Apps(a) = &self.active().data else {
                    return false;
                };
                a.items
                    .get(*i)
                    .map(|app| app.app_id == target_id)
                    .unwrap_or(false)
            } else {
                false
            }
        });
        if let Some(i) = new_sel
            && let TabData::Apps(a) = &mut self.active_mut().data
        {
            a.selected = i;
        }
    }

    /// Apply the "primary branches always, one latest auxiliary"
    /// filter to the raw branches for an app. Same shape the old
    /// App-tab view used.
    pub fn filtered_branches_for<'a>(a: &'a AppsTab, app_id: &str) -> Vec<&'a AmplifyBranch> {
        let Some(all) = a.branches_by_app.get(app_id) else {
            return Vec::new();
        };
        // 2026-07-03 UX pass: split branches three ways rather
        // than two.
        //   - essential — main/master/develop/staging/production:
        //     ALL of them show, in that canonical order.
        //   - releases  — release/*: only the ONE most-recently
        //     deployed. Repos with a long release/N.N.N history
        //     scan cleaner this way.
        //   - aux       — everything else (feature/*, bugfix/*):
        //     only the ONE most-recently deployed. Same as before.
        let is_essential =
            |n: &str| matches!(n, "main" | "master" | "develop" | "staging" | "production");
        let is_release = |n: &str| n.starts_with("release/");
        let mut essential: Vec<&AmplifyBranch> = Vec::new();
        let mut releases: Vec<&AmplifyBranch> = Vec::new();
        let mut aux: Vec<&AmplifyBranch> = Vec::new();
        for br in all {
            let n = br.branch_name.as_str();
            if is_essential(n) {
                essential.push(br);
            } else if is_release(n) {
                releases.push(br);
            } else {
                aux.push(br);
            }
        }
        essential.sort_by_key(|b| primary_sort_key(&b.branch_name));
        // "Most recently deployed" wins for both the release and
        // aux slots.
        let by_recent = |x: &&AmplifyBranch, y: &&AmplifyBranch| {
            let tx = last_deploy_ts_key(a, app_id, &x.branch_name);
            let ty = last_deploy_ts_key(a, app_id, &y.branch_name);
            ty.cmp(&tx)
        };
        releases.sort_by(by_recent);
        aux.sort_by(by_recent);
        // 2026-07-20 staleness filter — drop the release + aux
        // finalists if their last activity is beyond
        // STALE_AFTER_DAYS. Activity source, in order:
        //   1. jobs cache (`last_deploy_ts_key`) — freshest signal
        //      once list-jobs has run for this branch;
        //   2. AWS's `updateTime` on the branch (returned by
        //      list-branches; always present).
        // Essential branches are exempt — they're eternal envs.
        let is_stale = |b: &&AmplifyBranch| -> bool {
            let ts_from_jobs = last_deploy_ts_key(a, app_id, &b.branch_name);
            let ts = if ts_from_jobs.is_empty() {
                b.update_time.as_deref()
            } else {
                Some(ts_from_jobs.as_str())
            };
            ts.and_then(days_since_iso)
                .map(|d| d > STALE_AFTER_DAYS)
                .unwrap_or(false)
        };
        let mut out = essential;
        if let Some(b) = releases.into_iter().find(|b| !is_stale(b)) {
            out.push(b);
        }
        if let Some(b) = aux.into_iter().find(|b| !is_stale(b)) {
            out.push(b);
        }
        out
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
            self.refresh_active();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        // Clamp against the visible-row projection so the cursor
        // never lands on a filtered-out entry.
        let n = self.visible_rows().len();
        let TabData::Apps(a) = &mut self.active_mut().data else {
            return;
        };
        if n == 0 {
            return;
        }
        let n = n as isize;
        a.selected = ((a.selected as isize + delta).clamp(0, n - 1)) as usize;
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        match spec.kind {
            TabKind::Apps => {
                self.status = format!("refreshing {name}…");
                let rx = amplify::spawn_list_apps(spec.region.clone());
                let region = spec.region.clone();
                if let TabData::Apps(a) = &mut self.tabs[idx].data {
                    a.loading = true;
                    a.last_error = None;
                    a.pending = Some(rx);
                    // Also re-poll list-jobs for every branch we
                    // already know about — otherwise the row-view's
                    // "current" / "last deploy" columns stay frozen
                    // at whatever the first fan-out saw, so a build
                    // kicked off after startup only appears when the
                    // user drills in. Cheap: one list-jobs per known
                    // branch per refresh.
                    //
                    // Skip pairs that already have an in-flight
                    // request in `pending_jobs`. Without this guard,
                    // if AWS throttles / slows a batch and the next
                    // 60s tick (or an `r` press) fires before the
                    // prior batch drains, we double-stack the same
                    // (app_id, branch) fan-out. Worse: `drain` step 3
                    // does an unconditional insert into
                    // `jobs_by_key`, so an older reply landing after
                    // a newer one silently clobbers fresh data with
                    // stale — a build that just finished would flip
                    // back to RUNNING in the row until the following
                    // tick papers over it. Mirrors the
                    // `pending_branches.contains_key` idiom in
                    // `toggle_expand`.
                    // 2026-08-01 — pending_jobs now stores
                    // (app_id, branch_name, rx) so the in-flight
                    // guard is per-branch instead of the earlier
                    // "skip the whole app" fallback (which stalled
                    // slow apps for whole cycles). User: rows going
                    // in and out of populated state across ticks.
                    let in_flight: std::collections::HashSet<(String, String)> = a
                        .pending_jobs
                        .iter()
                        .map(|(app_id, branch_name, _)| {
                            (app_id.clone(), branch_name.clone())
                        })
                        .collect();
                    let known: Vec<(String, String)> = a
                        .branches_by_app
                        .iter()
                        .flat_map(|(app_id, brs)| {
                            brs.iter()
                                .map(move |b| (app_id.clone(), b.branch_name.clone()))
                        })
                        .filter(|k| !in_flight.contains(k))
                        .collect();
                    for (app_id, branch_name) in known {
                        let rx = amplify::spawn_list_jobs(
                            app_id.clone(),
                            branch_name.clone(),
                            region.clone(),
                        );
                        a.pending_jobs.push((app_id, branch_name, rx));
                    }
                }
            }
            TabKind::App => {
                self.status = format!("refreshing {name}…");
                let app_id = spec.app_id.clone().unwrap_or_default();
                let rx = amplify::spawn_list_branches(app_id, spec.region.clone());
                if let TabData::App(a) = &mut self.tabs[idx].data {
                    a.loading = true;
                    a.last_error = None;
                    a.pending = Some(rx);
                }
            }
        }
    }

    pub fn drain(&mut self) -> bool {
        let mut any = false;
        let region = self.tabs[0].spec.region.clone();
        let hidden = self.cfg.hidden_app_ids.clone();
        let TabData::Apps(a) = &mut self.tabs[0].data else {
            return false;
        };
        // 1. list-apps arrival.
        if let Some(rx) = a.pending.take() {
            let mut still_pending = true;
            match rx.try_recv() {
                Ok(AmplifyEvent::Apps(apps)) => {
                    any = true;
                    still_pending = false;
                    let total = apps.len();
                    let visible_count = apps
                        .iter()
                        .filter(|app| !hidden.iter().any(|h| h == &app.app_id))
                        .count();
                    // 2026-07-03 default expand-all: on first
                    // list-apps arrival, pre-expand every non-
                    // hidden app so the branches show up without
                    // the user having to walk the list. Only fires
                    // when we've never populated items before —
                    // subsequent refreshes leave the user's
                    // manual expand/collapse state alone.
                    let first_arrival = a.items.is_empty();
                    a.items = apps;
                    // 2026-07-19 rescue path — if the user has
                    // hidden every app (visible_count == 0 but
                    // total > 0), auto-flip `show_hidden` on so
                    // they see SOMETHING and can press X to
                    // unhide-all (or `x` on a row to unhide just
                    // that one). Without this, opening the sibling
                    // after "accidentally hid all" shows a blank
                    // pane with no obvious path forward — user
                    // report 2026-07-19.
                    if total > 0 && visible_count == 0 {
                        a.show_hidden = true;
                    }
                    if first_arrival {
                        for app in &a.items {
                            let is_hidden = hidden.iter().any(|h| h == &app.app_id);
                            if !is_hidden {
                                a.expanded.insert(app.app_id.clone());
                                // Fire list-branches so the
                                // pre-expanded rows populate.
                                let rx = amplify::spawn_list_branches(
                                    app.app_id.clone(),
                                    region.clone(),
                                );
                                a.pending_branches.insert(app.app_id.clone(), rx);
                            }
                        }
                    }
                    a.loading = false;
                    a.last_error = None;
                    a.last_fetched = Some(std::time::Instant::now());
                    let hidden_note = if total > visible_count {
                        format!(" ({} hidden)", total - visible_count)
                    } else {
                        String::new()
                    };
                    self.status = format!("{visible_count} apps{hidden_note}");
                }
                Ok(AmplifyEvent::Failed(e)) => {
                    any = true;
                    still_pending = false;
                    a.last_error = Some(e.clone());
                    a.loading = false;
                    self.status = format!("error: {e}");
                }
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    still_pending = false;
                }
            }
            if still_pending {
                let TabData::Apps(a) = &mut self.tabs[0].data else {
                    return any;
                };
                a.pending = Some(rx);
            }
        }
        // 2. Per-app list-branches arrivals. Buffer new fan-out
        //    targets so we can spawn list-jobs outside the mut
        //    borrow.
        let TabData::Apps(a) = &mut self.tabs[0].data else {
            return any;
        };
        let mut new_branch_fanout: Vec<(String, String)> = Vec::new();
        let mut delivered_branches: Vec<(String, Vec<AmplifyBranch>)> = Vec::new();
        a.pending_branches.retain(|app_id, rx| match rx.try_recv() {
            Ok(AmplifyEvent::Branches(brs)) => {
                delivered_branches.push((app_id.clone(), brs));
                false
            }
            Ok(AmplifyEvent::Failed(_)) | Ok(_) => false,
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        for (app_id, brs) in delivered_branches {
            any = true;
            for br in &brs {
                new_branch_fanout.push((app_id.clone(), br.branch_name.clone()));
            }
            a.branches_by_app.insert(app_id, brs);
        }
        // 3. Per-branch list-jobs arrivals — route by (app_id,
        //    branch_name) where app_id is stashed in the fan-out
        //    tuple. AmplifyEvent::Jobs carries branch_name but
        //    NOT app_id, so we recover app_id from the local
        //    pending tuple.
        let mut delivered_jobs: Vec<(String, String, Vec<AmplifyJob>)> = Vec::new();
        let mut delivered_job_errors: Vec<(String, String, String)> = Vec::new();
        a.pending_jobs
            .retain_mut(|(app_id, branch_name, rx)| match rx.try_recv() {
                Ok(AmplifyEvent::Jobs { branch_name, jobs }) => {
                    delivered_jobs.push((app_id.clone(), branch_name, jobs));
                    false
                }
                Ok(AmplifyEvent::Failed(e)) => {
                    delivered_job_errors.push((app_id.clone(), branch_name.clone(), e));
                    false
                }
                Ok(_) => false,
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            });
        for (app_id, branch_name, jobs) in delivered_jobs {
            any = true;
            // 2026-08-01 — was unconditional `insert`, which
            // clobbered a previously-populated row when AWS
            // returned an empty vec on a transient hiccup (throttle,
            // brief perms glitch). User watched rows go
            // `#1200 SUCCEED` → `…` between refreshes.
            //
            // New rule: an empty reply NEVER clobbers a non-empty
            // cache entry. First-time empty replies still populate
            // (so "genuinely no deploys" rows correctly show empty).
            let key = (app_id, branch_name);
            let existing_has_data =
                a.jobs_by_key.get(&key).is_some_and(|v| !v.is_empty());
            if !jobs.is_empty() || !existing_has_data {
                a.jobs_by_key.insert(key.clone(), jobs);
            }
            // Clear any stale error — the request just succeeded.
            a.jobs_error_by_key.remove(&key);
        }
        for (app_id, branch_name, err) in delivered_job_errors {
            any = true;
            // Only record the error if we don't already have cached
            // data for this branch — otherwise the row keeps showing
            // its last known good state (with the periodic refresh
            // eventually retrying) instead of jumping to a scary red
            // error and back. But if the row is empty AND expanded,
            // the user is staring at "(loading…)" so we need the
            // error visible.
            let key = (app_id, branch_name);
            if !a.jobs_by_key.get(&key).is_some_and(|v| !v.is_empty()) {
                a.jobs_error_by_key.insert(key, err);
            }
        }
        // 4. Fan out list-jobs for the branches that just landed.
        for (app_id, branch_name) in new_branch_fanout {
            let rx = amplify::spawn_list_jobs(
                app_id.clone(),
                branch_name.clone(),
                region.clone(),
            );
            a.pending_jobs.push((app_id, branch_name, rx));
        }
        // 5. Deployment-history overlay drain.
        self.drain_deployment_history(&mut any);
        // 6. Logs overlay drain.
        self.drain_logs_view(&mut any);
        any
    }

    fn drain_deployment_history(&mut self, any: &mut bool) {
        let Some(dh) = &mut self.deployment_history else {
            return;
        };
        let Some(rx) = dh.pending_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(AmplifyEvent::Jobs { jobs, .. }) => {
                *any = true;
                dh.jobs = jobs;
                dh.loading = false;
                if dh.selected >= dh.jobs.len() {
                    dh.selected = dh.jobs.len().saturating_sub(1);
                }
            }
            Ok(AmplifyEvent::Failed(e)) => {
                *any = true;
                dh.loading = false;
                dh.error = Some(e);
            }
            Ok(_) => {}
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                dh.pending_rx = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                dh.loading = false;
            }
        }
    }

    fn drain_logs_view(&mut self, any: &mut bool) {
        let Some(lv) = &mut self.logs_view else {
            return;
        };
        // 1. Job detail arrival. On success we fan out one log
        //    fetch per step (skipping steps with no log URL).
        if let Some(rx) = lv.detail_rx.take() {
            let mut still_pending = true;
            match rx.try_recv() {
                Ok(AmplifyEvent::JobDetail(detail)) => {
                    *any = true;
                    still_pending = false;
                    lv.loading_detail = false;
                    for step in detail.steps {
                        if let Some(url) = step.log_url.clone() {
                            let rx = amplify::spawn_fetch_log(step.step_name.clone(), url);
                            lv.log_rxs.push(rx);
                            lv.steps.push((step.step_name.clone(), String::new()));
                        } else {
                            lv.steps.push((
                                step.step_name.clone(),
                                format!("(no log URL for step; status={})", step.status),
                            ));
                        }
                    }
                }
                Ok(AmplifyEvent::Failed(e)) => {
                    *any = true;
                    still_pending = false;
                    lv.loading_detail = false;
                    lv.error = Some(e);
                }
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    still_pending = false;
                }
            }
            if still_pending {
                lv.detail_rx = Some(rx);
            }
        }
        // 2. Per-step log arrivals. `retain_mut` drops receivers
        //    that either delivered a message or disconnected.
        let mut delivered: Vec<(String, String)> = Vec::new();
        lv.log_rxs.retain_mut(|rx| match rx.try_recv() {
            Ok(AmplifyEvent::Log { step_name, text }) => {
                delivered.push((step_name, text));
                false
            }
            Ok(AmplifyEvent::Failed(e)) => {
                delivered.push(("(unknown)".to_string(), format!("log fetch failed: {e}")));
                false
            }
            Ok(_) => false,
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        if !delivered.is_empty() {
            *any = true;
        }
        for (step_name, text) in delivered {
            if let Some(slot) = lv.steps.iter_mut().find(|(name, _)| name == &step_name) {
                slot.1 = text;
            } else {
                lv.steps.push((step_name, text));
            }
        }
    }

    /// Called from key handling — Esc / q in the logs overlay.
    pub fn close_logs_view(&mut self) {
        self.logs_view = None;
    }

    /// Called from key handling when the logs overlay is open.
    pub fn logs_scroll(&mut self, delta: i32) {
        let Some(lv) = &mut self.logs_view else {
            return;
        };
        lv.scroll = (lv.scroll as i32 + delta).max(0) as u16;
    }

    /// Currently-focused app in the "All apps" tab, respecting
    /// hidden_app_ids + show_hidden. Returns `(idx_in_items, &app)`.
    pub fn focused_app(&self) -> Option<(usize, &AmplifyApp)> {
        let TabData::Apps(a) = &self.active().data else {
            return None;
        };
        let hidden = &self.cfg.hidden_app_ids;
        let mut visible_seen = 0usize;
        for (i, app) in a.items.iter().enumerate() {
            let is_hidden = hidden.iter().any(|h| h == &app.app_id);
            if is_hidden && !a.show_hidden {
                continue;
            }
            if visible_seen == a.selected {
                return Some((i, app));
            }
            visible_seen += 1;
        }
        None
    }

    /// Number of rows that will render on the Apps tab under the
    /// current show_hidden setting. Used to clamp selection after
    /// hide/unhide.
    fn apps_visible_len(&self) -> usize {
        let TabData::Apps(a) = &self.active().data else {
            return 0;
        };
        if a.show_hidden {
            a.items.len()
        } else {
            let hidden = &self.cfg.hidden_app_ids;
            a.items
                .iter()
                .filter(|app| !hidden.iter().any(|h| h == &app.app_id))
                .count()
        }
    }

    /// `X` (shift-x) — clear the entire hidden list. Rescue path
    /// for "I accidentally hid all my apps and can't get them
    /// back". Persists the empty list. tree-redesign 2026-07-19.
    pub fn unhide_all(&mut self) {
        let n = self.cfg.hidden_app_ids.len();
        if n == 0 {
            self.status = "no hidden apps to restore".into();
            return;
        }
        self.cfg.hidden_app_ids.clear();
        if let TabData::Apps(a) = &mut self.active_mut().data {
            a.show_hidden = false;
        }
        if let Err(e) = crate::config::save(&self.cfg) {
            self.status = format!("restored {n} apps in memory (save failed: {e})");
        } else {
            self.status = format!("restored {n} previously-hidden apps");
        }
    }

    /// `x` — toggle the hide-state of the focused app. Persists
    /// `hidden_app_ids` to the config file so the change survives
    /// restart. Silent no-op on the App tab (branches don't hide).
    pub fn toggle_hide_selected(&mut self) {
        let Some((_, app)) = self.focused_app() else {
            self.status = "no app selected to hide".into();
            return;
        };
        let target_id = app.app_id.clone();
        let target_name = app.name.clone();
        let was_hidden = self.cfg.hidden_app_ids.iter().any(|h| h == &target_id);
        if was_hidden {
            self.cfg.hidden_app_ids.retain(|h| h != &target_id);
        } else {
            self.cfg.hidden_app_ids.push(target_id);
        }
        // Persist. Failures fall to the status line so users know
        // their toggle didn't survive to disk.
        if let Err(e) = crate::config::save(&self.cfg) {
            self.status = format!("hide toggled in memory (save failed: {e})");
        } else {
            self.status = if was_hidden {
                format!("unhidden {target_name}  (H to show/hide the hidden view)")
            } else {
                format!("hidden {target_name}  (H to see hidden, `x` to unhide)")
            };
        }
        // Selection may now overshoot the visible list — clamp.
        let visible_len = self.apps_visible_len();
        if let TabData::Apps(a) = &mut self.active_mut().data
            && a.selected >= visible_len
        {
            a.selected = visible_len.saturating_sub(1);
        }
    }

    /// `H` — flip show-hidden mode. Adjusts selection so a hidden
    /// row that just appeared doesn't get skipped.
    pub fn toggle_show_hidden(&mut self) {
        let TabData::Apps(a) = &mut self.active_mut().data else {
            self.status = "show-hidden only applies to the Apps tab".into();
            return;
        };
        a.show_hidden = !a.show_hidden;
        let now_on = a.show_hidden;
        // Reset selection to top so the mode change is visually clear.
        a.selected = 0;
        self.status = if now_on {
            "show-hidden ON  (hidden apps appear dimmed; `x` to unhide)".into()
        } else {
            "show-hidden OFF  (hidden apps filtered)".into()
        };
    }

    /// `Enter` — context-aware.
    /// - App-header row: toggle expanded. On first expand, kick
    ///   off a `list-branches` fetch for that app.
    /// - Branch row: toggle an inline last-deploy detail block
    ///   below the row (job # + status + duration + commit +
    ///   started-at). Mirrors mnml-forge-bitbucket's merged-PR
    ///   expand. 2026-08-01.
    pub fn enter_focused(&mut self) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected_index()).cloned() else {
            self.status = "nothing under cursor".into();
            return;
        };
        match row {
            VisibleRow::AppHeader { items_index, .. } => {
                self.toggle_expand(items_index);
            }
            VisibleRow::Branch {
                app_id,
                branch_name,
            } => {
                // 2026-08-01 — Enter on a branch row opens the
                // deployment-history drill-in (was briefly repurposed
                // for expand/collapse — user asked to revert). The
                // expand toggle now lives on Space + on chevron click
                // + on Right/Left arrows.
                self.open_deployment_history(&app_id, &branch_name);
            }
            VisibleRow::Deployment {
                app_id,
                branch_name,
                job_id,
            } => {
                // Enter on a Deployment row (an inline row in an
                // expanded branch) drills straight into that specific
                // job's logs — matches clicking the row.
                self.open_logs_for_job(&app_id, &branch_name, &job_id);
            }
        }
    }

    /// Toggle expand on the currently-focused branch row without
    /// opening the drill-in. Called by chevron mouse click + Space
    /// keyboard binding.
    pub fn toggle_branch_expand_selected(&mut self) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected_index()).cloned() else {
            return;
        };
        if let VisibleRow::Branch {
            app_id,
            branch_name,
        } = row
        {
            self.toggle_branch_expand(&app_id, &branch_name);
        }
    }

    /// Toggle the inline expand on a branch row without opening the
    /// drill-in. Bound to Space, ▶/▼ chevron click, and Right/Left.
    pub fn toggle_branch_expand(&mut self, app_id: &str, branch_name: &str) {
        let key = (app_id.to_string(), branch_name.to_string());
        if self.expanded_branches.contains(&key) {
            self.expanded_branches.remove(&key);
            self.status = format!("collapsed {branch_name}");
        } else {
            self.expanded_branches.insert(key.clone());
            self.status = format!("expanded {branch_name}");
            // Eager fetch on first expand — otherwise the user waits
            // for the next 60s refresh tick to see anything. Skip if
            // the branch already has cached jobs or an in-flight
            // request; also clear any stale error so the retry has a
            // clean slate.
            let region = self.tabs[self.active_tab].spec.region.clone();
            if let TabData::Apps(a) = &mut self.tabs[self.active_tab].data {
                let has_data = a
                    .jobs_by_key
                    .get(&key)
                    .is_some_and(|v| !v.is_empty());
                let in_flight = a
                    .pending_jobs
                    .iter()
                    .any(|(a_id, b_name, _)| a_id == &key.0 && b_name == &key.1);
                if !has_data && !in_flight {
                    a.jobs_error_by_key.remove(&key);
                    let rx = amplify::spawn_list_jobs(
                        key.0.clone(),
                        key.1.clone(),
                        region.clone(),
                    );
                    a.pending_jobs.push((key.0, key.1, rx));
                }
            }
        }
    }

    fn selected_index(&self) -> usize {
        let TabData::Apps(a) = &self.active().data else {
            return 0;
        };
        a.selected
    }

    /// `E` — expand every visible (non-hidden) app. Kicks off
    /// branch fetches for those that haven't been loaded yet.
    pub fn expand_all(&mut self) {
        let region = self.active().spec.region.clone();
        let hidden = self.cfg.hidden_app_ids.clone();
        let TabData::Apps(a) = &mut self.active_mut().data else {
            return;
        };
        let mut new_fetches: Vec<String> = Vec::new();
        let visible_ids: Vec<String> = a
            .items
            .iter()
            .filter(|app| a.show_hidden || !hidden.iter().any(|h| h == &app.app_id))
            .map(|app| app.app_id.clone())
            .collect();
        for id in &visible_ids {
            if a.expanded.insert(id.clone())
                && !a.branches_by_app.contains_key(id)
                && !a.pending_branches.contains_key(id)
            {
                new_fetches.push(id.clone());
            }
        }
        for id in new_fetches {
            let rx = amplify::spawn_list_branches(id.clone(), region.clone());
            a.pending_branches.insert(id, rx);
        }
        self.status = format!("expanded {} apps", visible_ids.len());
    }

    /// `C` — collapse every app.
    pub fn collapse_all(&mut self) {
        let hidden = self.cfg.hidden_app_ids.clone();
        let TabData::Apps(a) = &mut self.active_mut().data else {
            return;
        };
        a.expanded.clear();
        // Cursor may now point past the visible list — clamp.
        let visible_len = a
            .items
            .iter()
            .filter(|app| a.show_hidden || !hidden.iter().any(|h| h == &app.app_id))
            .count()
            .max(1);
        if a.selected >= visible_len {
            a.selected = visible_len.saturating_sub(1);
        }
        self.status = "collapsed all".into();
    }

    /// `→` — if the focused row is a collapsed app header, expand
    /// it (and kick off `list-branches` if we haven't fetched yet).
    /// On a branch row, expand its inline last-deploy detail block
    /// (2026-08-01). No-op on already-expanded rows.
    pub fn expand_focused(&mut self) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected_index()).cloned() else {
            return;
        };
        match row {
            VisibleRow::AppHeader {
                items_index,
                expanded: false,
            } => {
                self.toggle_expand(items_index);
            }
            VisibleRow::AppHeader { expanded: true, .. } => {}
            VisibleRow::Branch {
                app_id,
                branch_name,
            } => {
                let key = (app_id.clone(), branch_name.clone());
                if !self.expanded_branches.contains(&key) {
                    self.toggle_branch_expand(&app_id, &branch_name);
                }
            }
            VisibleRow::Deployment { .. } => {
                // Deployment rows are already "leaves" — nothing
                // to expand further. Enter/click drills into logs.
            }
        }
    }

    /// `←` — if the focused row is an expanded app header, collapse
    /// it. On an expanded branch row (2026-08-01), collapse the
    /// inline detail block. On a collapsed branch row, jump to the
    /// parent app header (matches most tree UIs like VS Code's
    /// file tree).
    pub fn collapse_focused(&mut self) {
        let rows = self.visible_rows();
        let sel = self.selected_index();
        let Some(row) = rows.get(sel).cloned() else {
            return;
        };
        match row {
            VisibleRow::AppHeader {
                items_index,
                expanded: true,
            } => {
                self.toggle_expand(items_index);
            }
            VisibleRow::AppHeader {
                expanded: false, ..
            } => {
                // Already collapsed — no-op.
            }
            VisibleRow::Branch {
                app_id,
                branch_name,
            } => {
                let key = (app_id.clone(), branch_name.clone());
                if self.expanded_branches.contains(&key) {
                    // Collapse the inline detail block first.
                    self.toggle_branch_expand(&app_id, &branch_name);
                    return;
                }
                // Walk up to the header for this app_id.
                let mut new_sel = sel;
                for (i, r) in rows.iter().enumerate().take(sel) {
                    if let VisibleRow::AppHeader { items_index, .. } = r {
                        // We know the header comes before its branches;
                        // pick the one whose items_index matches app_id.
                        let TabData::Apps(a) = &self.active().data else {
                            return;
                        };
                        if a.items.get(*items_index).map(|app| &app.app_id) == Some(&app_id) {
                            new_sel = i;
                        }
                    }
                }
                if let TabData::Apps(a) = &mut self.active_mut().data {
                    a.selected = new_sel;
                }
            }
            VisibleRow::Deployment {
                app_id,
                branch_name,
                ..
            } => {
                // Left on a Deployment row: jump up to its parent
                // Branch row (mirrors "collapse walks up" idiom).
                let mut new_sel = sel;
                for (i, r) in rows.iter().enumerate().take(sel) {
                    if let VisibleRow::Branch {
                        app_id: a_id,
                        branch_name: b_name,
                    } = r
                    {
                        if a_id == &app_id && b_name == &branch_name {
                            new_sel = i;
                        }
                    }
                }
                if let TabData::Apps(a) = &mut self.active_mut().data {
                    a.selected = new_sel;
                }
            }
        }
    }

    fn toggle_expand(&mut self, items_index: usize) {
        let region = self.active().spec.region.clone();
        let TabData::Apps(a) = &mut self.active_mut().data else {
            return;
        };
        let Some(app) = a.items.get(items_index) else {
            return;
        };
        let app_id = app.app_id.clone();
        let name = app.name.clone();
        if a.expanded.contains(&app_id) {
            a.expanded.remove(&app_id);
            self.status = format!("collapsed {name}");
            return;
        }
        // Expanding — fetch branches if we haven't yet + they're
        // not already in-flight.
        a.expanded.insert(app_id.clone());
        let already_have = a.branches_by_app.contains_key(&app_id);
        let already_pending = a.pending_branches.contains_key(&app_id);
        if !already_have && !already_pending {
            let rx = amplify::spawn_list_branches(app_id.clone(), region);
            a.pending_branches.insert(app_id.clone(), rx);
            self.status = format!("expanded {name} — loading branches…");
        } else {
            self.status = format!("expanded {name}");
        }
    }

    /// Open the DeploymentHistoryView for a branch. Fires list-jobs
    /// even if we've already cached the jobs, so the view is fresh.
    /// Called from `enter_focused` on a branch row (Enter) instead
    /// of going straight to logs — user asked for the console-style
    /// history-then-drill flow.
    fn open_deployment_history(&mut self, app_id: &str, branch_name: &str) {
        let region = self.active().spec.region.clone();
        let app_name = {
            let TabData::Apps(a) = &self.active().data else {
                return;
            };
            a.items
                .iter()
                .find(|app| app.app_id == app_id)
                .map(|app| app.name.clone())
                .unwrap_or_else(|| app_id.to_string())
        };
        // If we already have jobs cached (from the fan-out) show
        // them immediately; still fire a refresh so the list is
        // current when it lands.
        let cached: Vec<AmplifyJob> = if let TabData::Apps(a) = &self.active().data {
            a.jobs_by_key
                .get(&(app_id.to_string(), branch_name.to_string()))
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let rx = amplify::spawn_list_jobs(app_id.to_string(), branch_name.to_string(), region);
        self.deployment_history = Some(DeploymentHistoryView {
            app_id: app_id.to_string(),
            app_name,
            branch_name: branch_name.to_string(),
            loading: cached.is_empty(),
            pending_rx: Some(rx),
            jobs: cached,
            selected: 0,
            scroll: 0,
            error: None,
        });
        self.status = format!("deployments · {branch_name}");
    }

    /// Close the deployment-history overlay (Esc / q).
    pub fn close_deployment_history(&mut self) {
        self.deployment_history = None;
    }

    /// Move the cursor within the deployment-history table.
    pub fn deployment_history_move(&mut self, delta: isize) {
        let Some(dh) = &mut self.deployment_history else {
            return;
        };
        if dh.jobs.is_empty() {
            return;
        }
        let n = dh.jobs.len() as isize;
        dh.selected = ((dh.selected as isize + delta).clamp(0, n - 1)) as usize;
    }

    /// Enter on a row inside DeploymentHistoryView — open the
    /// existing LogsView for THAT specific job (not just the
    /// latest).
    pub fn deployment_history_enter(&mut self) {
        let Some(dh) = &self.deployment_history else {
            return;
        };
        let Some(job) = dh.jobs.get(dh.selected) else {
            return;
        };
        let app_id = dh.app_id.clone();
        let branch_name = dh.branch_name.clone();
        let job_id = job.job_id.clone();
        let commit_msg = job.commit_message.clone();
        let region = self.active().spec.region.clone();
        let rx =
            amplify::spawn_get_job(app_id.clone(), branch_name.clone(), job_id.clone(), region);
        self.status = format!("fetching logs for {branch_name} #{job_id}…");
        self.logs_view = Some(LogsView {
            branch_name,
            job_id: Some(job_id),
            commit_message: commit_msg,
            loading_detail: true,
            detail_rx: Some(rx),
            log_rxs: Vec::new(),
            steps: Vec::new(),
            error: None,
            scroll: 0,
        });
    }

    fn open_logs_for_branch(&mut self, app_id: &str, branch_name: &str) {
        let region = self.active().spec.region.clone();
        let TabData::Apps(a) = &self.active().data else {
            return;
        };
        let jobs = a
            .jobs_by_key
            .get(&(app_id.to_string(), branch_name.to_string()))
            .cloned()
            .unwrap_or_default();
        let Some(latest) = jobs.first() else {
            self.status = format!("no deploys yet for {branch_name}");
            return;
        };
        let job_id = latest.job_id.clone();
        let commit_msg = latest.commit_message.clone();
        let rx = amplify::spawn_get_job(
            app_id.to_string(),
            branch_name.to_string(),
            job_id.clone(),
            region,
        );
        self.status = format!("fetching logs for {branch_name} #{job_id}…");
        self.logs_view = Some(LogsView {
            branch_name: branch_name.to_string(),
            job_id: Some(job_id),
            commit_message: commit_msg,
            loading_detail: true,
            detail_rx: Some(rx),
            log_rxs: Vec::new(),
            steps: Vec::new(),
            error: None,
            scroll: 0,
        });
    }

    /// Open the logs viewer for a specific job — used by clicks on
    /// the #JOB cell in a branch's expanded detail block. Same as
    /// `open_logs_for_branch` but for an arbitrary job_id, not just
    /// the latest.
    pub fn open_logs_for_job(
        &mut self,
        app_id: &str,
        branch_name: &str,
        job_id: &str,
    ) {
        let region = self.active().spec.region.clone();
        let commit_msg = {
            let TabData::Apps(a) = &self.active().data else {
                return;
            };
            a.jobs_by_key
                .get(&(app_id.to_string(), branch_name.to_string()))
                .and_then(|jobs| jobs.iter().find(|j| j.job_id == job_id))
                .and_then(|j| j.commit_message.clone())
        };
        let rx = amplify::spawn_get_job(
            app_id.to_string(),
            branch_name.to_string(),
            job_id.to_string(),
            region,
        );
        self.status = format!("fetching logs for {branch_name} #{job_id}…");
        self.logs_view = Some(LogsView {
            branch_name: branch_name.to_string(),
            job_id: Some(job_id.to_string()),
            commit_message: commit_msg,
            loading_detail: true,
            detail_rx: Some(rx),
            log_rxs: Vec::new(),
            steps: Vec::new(),
            error: None,
            scroll: 0,
        });
    }

    pub fn open_focused(&mut self) {
        let region = self.active().spec.region.clone();
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected_index()).cloned() else {
            self.status = "no row under cursor".into();
            return;
        };
        let url = match row {
            VisibleRow::AppHeader { items_index, .. } => {
                let TabData::Apps(a) = &self.active().data else {
                    return;
                };
                a.items
                    .get(items_index)
                    .map(|app| amplify::console_url_app(&app.app_id, region.as_deref()))
            }
            VisibleRow::Branch {
                app_id,
                branch_name,
            }
            | VisibleRow::Deployment {
                app_id,
                branch_name,
                ..
            } => Some(amplify::console_url_branch(
                &app_id,
                &branch_name,
                region.as_deref(),
            )),
        };
        let Some(url) = url else {
            self.status = "no URL for this row".into();
            return;
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `L` — cross-sibling jump: on a focused branch row inside an
    /// `app` tab, spawn `mnml-aws-cloudwatch-logs` scoped to the
    /// branch's deploy-log group (`/aws/amplify/<app_id>/<branch>`).
    /// Mirrors the lambda → cloudwatch-logs handoff pattern.
    pub fn handoff_logs(&mut self) {
        // 2026-07-03 unified view — handoff resolves the focused
        // (app_id, branch_name) directly from the visible-row
        // projection. Only valid on branch rows.
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected_index()).cloned() else {
            self.status = "no row under cursor".into();
            return;
        };
        let (app_id, branch_name) = match row {
            VisibleRow::Branch {
                app_id,
                branch_name,
            }
            | VisibleRow::Deployment {
                app_id,
                branch_name,
                ..
            } => (app_id, branch_name),
            VisibleRow::AppHeader { .. } => {
                self.status = "L opens CloudWatch Logs — pick a branch row first".into();
                return;
            }
        };
        // Amplify only creates `/aws/amplify/<app_id>/<branch>` on the
        // first build. Branches that have never deployed have no log
        // group — spawning cloudwatch-logs there just shows a
        // `ResourceNotFoundException` in a Pty pane, which reads as
        // a bug. Bail with a helpful status instead.
        let has_deploys = if let TabData::Apps(a) = &self.active().data {
            a.jobs_by_key
                .get(&(app_id.clone(), branch_name.clone()))
                .is_some_and(|jobs| !jobs.is_empty())
        } else {
            false
        };
        if !has_deploys {
            self.status = format!("no builds yet on `{branch_name}` — no CloudWatch log group");
            return;
        }
        let log_group = format!("/aws/amplify/{app_id}/{branch_name}");
        let region = self.active().spec.region.clone();
        let mut cmd = std::process::Command::new("mnml-aws-cloudwatch-logs");
        cmd.args(["--log-group", &log_group, "--log-group-name", &branch_name]);
        if let Some(r) = &region {
            cmd.args(["--region", r]);
        }
        match cmd.spawn() {
            Ok(_) => {
                self.status = format!("launched cloudwatch-logs → {log_group}");
            }
            Err(e) => {
                self.status =
                    format!("spawn failed (install mnml-aws-cloudwatch-logs ≥ v0.2.0): {e}");
            }
        }
    }

    pub fn yank_focused_url(&mut self) {
        let region = self.active().spec.region.clone();
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected_index()).cloned() else {
            self.status = "no row under cursor".into();
            return;
        };
        let url = match row {
            VisibleRow::AppHeader { items_index, .. } => {
                let TabData::Apps(a) = &self.active().data else {
                    return;
                };
                a.items
                    .get(items_index)
                    .map(|app| amplify::console_url_app(&app.app_id, region.as_deref()))
            }
            VisibleRow::Branch {
                app_id,
                branch_name,
            }
            | VisibleRow::Deployment {
                app_id,
                branch_name,
                ..
            } => Some(amplify::console_url_branch(
                &app_id,
                &branch_name,
                region.as_deref(),
            )),
        };
        let Some(url) = url else {
            self.status = "no URL for this row".into();
            return;
        };
        match crate::clipboard::copy(&url) {
            Ok(()) => self.status = format!("copied {url}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }
}
