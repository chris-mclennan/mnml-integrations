//! App state — per-tab list of RDS items (DB instances OR Aurora
//! clusters) + a selection cursor.

use crate::config::{Config, Tab};
use crate::rds::{self, Item};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub region: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match t.kind.as_str() {
            "instances" | "clusters" => Ok(Self {
                kind: t.kind.clone(),
                region,
            }),
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
            "instances" => rds::list_db_instances(spec.region.as_deref())
                .map(|xs| xs.into_iter().map(Item::Instance).collect()),
            "clusters" => rds::list_db_clusters(spec.region.as_deref())
                .map(|xs| xs.into_iter().map(Item::Cluster).collect()),
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
                    "instances" => "instances",
                    "clusters" => "clusters",
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

    /// `o` — open the RDS console URL for the focused item.
    pub fn open_console(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let region = self.active().spec.region.as_deref().unwrap_or("us-east-1");
        let url = match item {
            Item::Instance(i) => format!(
                "https://{region}.console.aws.amazon.com/rds/home?region={region}#database:id={};is-cluster=false",
                i.identifier
            ),
            Item::Cluster(c) => format!(
                "https://{region}.console.aws.amazon.com/rds/home?region={region}#database:id={};is-cluster=true",
                c.identifier
            ),
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` — yank focused item's ARN to clipboard.
    pub fn yank_arn(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let arn = item.arn().to_string();
        match crate::clipboard::copy(&arn) {
            Ok(()) => self.status = format!("copied ARN ({} chars)", arn.len()),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `E` — yank the focused item's endpoint (host:port) to clipboard.
    /// Useful for `psql` / `mysql` / etc. wiring.
    pub fn yank_endpoint(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Some(endpoint) = item.endpoint() else {
            self.status = "no endpoint exposed for the focused item".into();
            return;
        };
        match crate::clipboard::copy(&endpoint) {
            Ok(()) => self.status = format!("copied endpoint: {endpoint}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `L` — spawn `mnml-aws-cloudwatch-logs` scoped to the focused
    /// instance's primary log group. The log-group convention is
    /// engine-specific; we pick the most common per-engine default:
    ///
    /// - `postgres` / `aurora-postgresql` → `/aws/rds/instance/<id>/postgresql`
    /// - `mysql` / `mariadb` / `aurora-mysql` → `/aws/rds/instance/<id>/error`
    /// - `oracle-*` → `/aws/rds/instance/<id>/trace`
    /// - `sqlserver-*` → `/aws/rds/instance/<id>/error`
    /// - anything else → `/aws/rds/instance/<id>/error`
    ///
    /// Requires mnml-aws-cloudwatch-logs ≥ v0.2.0 (the version that
    /// added the `--log-group` flag). For cluster items we use the
    /// cluster identifier — Aurora streams logs to log groups named
    /// after each cluster member, but most users want a quick
    /// starting point.
    pub fn tail_logs(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let (identifier, engine, label) = match item {
            Item::Instance(i) => {
                let id = i.identifier.clone();
                let engine = i.engine.clone();
                let label = id.clone();
                (id, engine, label)
            }
            Item::Cluster(c) => {
                let id = c.identifier.clone();
                let engine = c.engine.clone();
                let label = format!("{id} (cluster)");
                (id, engine, label)
            }
        };
        let log_group = log_group_for(&identifier, engine.as_deref());
        let region = self.active().spec.region.clone();

        let mut cmd = std::process::Command::new("mnml-aws-cloudwatch-logs");
        cmd.args(["--log-group", &log_group, "--log-group-name", &label]);
        if let Some(r) = &region {
            cmd.args(["--region", r]);
        }
        match cmd.spawn() {
            Ok(_) => {
                self.status = format!("tailing {log_group}");
            }
            Err(e) => {
                self.status =
                    format!("spawn failed (install mnml-aws-cloudwatch-logs ≥ v0.2.0): {e}");
            }
        }
    }

    /// `D` — database handoff. Reads the focused instance/cluster's
    /// `engine` and spawns the matching mnml-db-* sibling so the user
    /// can connect to the actual database. Auto-connect via endpoint
    /// is v0.x — today the user fills in the connection details
    /// inside the spawned sibling.
    pub fn handoff_db(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let (engine, label) = match item {
            Item::Instance(i) => (i.engine.clone(), i.identifier.clone()),
            Item::Cluster(c) => (c.engine.clone(), c.identifier.clone()),
        };
        let engine_lc = engine.as_deref().unwrap_or("").to_ascii_lowercase();
        let binary =
            if engine_lc.starts_with("postgres") || engine_lc.starts_with("aurora-postgres") {
                "mnml-db-postgres"
            } else if engine_lc.starts_with("mariadb")
                || engine_lc.starts_with("mysql")
                || engine_lc.starts_with("aurora-mysql")
            {
                "mnml-db-mariadb"
            } else {
                self.status = format!(
                    "no db sibling for engine `{}` — supported: postgres, mariadb, mysql",
                    engine.unwrap_or_default()
                );
                return;
            };

        match std::process::Command::new(binary).spawn() {
            Ok(_) => {
                self.status =
                    format!("launched {binary} — connect to {label} (auto-connect is v0.x)");
            }
            Err(e) => {
                self.status = format!("spawn {binary} failed (install it?): {e}");
            }
        }
    }
}

/// Derive the most common log group for an RDS instance given its
/// engine. AWS RDS publishes engine-specific log streams to log
/// groups of the form `/aws/rds/instance/<id>/<stream>`. The stream
/// name depends on engine:
///   - postgres family → `postgresql`
///   - mysql / mariadb family → `error`
///   - oracle family → `trace`
///   - sqlserver family → `error`
pub fn log_group_for(identifier: &str, engine: Option<&str>) -> String {
    let stream = match engine.unwrap_or("").to_ascii_lowercase().as_str() {
        e if e.starts_with("postgres") || e.starts_with("aurora-postgres") => "postgresql",
        e if e.starts_with("mysql")
            || e.starts_with("mariadb")
            || e.starts_with("aurora-mysql") =>
        {
            "error"
        }
        e if e.starts_with("oracle") => "trace",
        e if e.starts_with("sqlserver") => "error",
        _ => "error",
    };
    format!("/aws/rds/instance/{identifier}/{stream}")
}

#[cfg(test)]
mod log_group_tests {
    use super::*;

    #[test]
    fn postgres_engine_maps_to_postgresql_stream() {
        assert_eq!(
            log_group_for("prod-db", Some("postgres")),
            "/aws/rds/instance/prod-db/postgresql"
        );
        assert_eq!(
            log_group_for("prod-db", Some("aurora-postgresql")),
            "/aws/rds/instance/prod-db/postgresql"
        );
    }

    #[test]
    fn mysql_engine_maps_to_error_stream() {
        assert_eq!(
            log_group_for("prod-db", Some("mysql")),
            "/aws/rds/instance/prod-db/error"
        );
        assert_eq!(
            log_group_for("prod-db", Some("aurora-mysql")),
            "/aws/rds/instance/prod-db/error"
        );
        assert_eq!(
            log_group_for("prod-db", Some("mariadb")),
            "/aws/rds/instance/prod-db/error"
        );
    }

    #[test]
    fn oracle_maps_to_trace_stream() {
        assert_eq!(
            log_group_for("prod-db", Some("oracle-ee")),
            "/aws/rds/instance/prod-db/trace"
        );
    }

    #[test]
    fn unknown_engine_falls_back_to_error() {
        assert_eq!(
            log_group_for("prod-db", Some("custom-engine")),
            "/aws/rds/instance/prod-db/error"
        );
        assert_eq!(
            log_group_for("prod-db", None),
            "/aws/rds/instance/prod-db/error"
        );
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
            kind: "instances".into(),
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_unknown_kind() {
        let t = Tab {
            name: "bad".into(),
            kind: "garbage".into(),
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }
}
