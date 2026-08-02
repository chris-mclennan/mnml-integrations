//! App state — per-tab list of ECS items (clusters OR services) +
//! a selection cursor.

use crate::config::{Config, Tab};
use crate::ecs::{self, Item};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub cluster: Option<String>,
    pub region: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match t.kind.as_str() {
            "clusters" => Ok(Self {
                kind: "clusters".into(),
                cluster: None,
                region,
            }),
            "services" => {
                let cluster = t.cluster.clone().unwrap_or_default();
                if cluster.trim().is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"services\" requires `cluster`", t.name);
                }
                Ok(Self {
                    kind: "services".into(),
                    cluster: Some(cluster),
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
            "clusters" => ecs::list_clusters(spec.region.as_deref())
                .map(|cs| cs.into_iter().map(Item::Cluster).collect()),
            "services" => {
                let cluster = spec
                    .cluster
                    .as_deref()
                    .expect("services tab requires cluster (validated)");
                ecs::list_services(cluster, spec.region.as_deref())
                    .map(|ss| ss.into_iter().map(Item::Service).collect())
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
                    "clusters" => "clusters",
                    "services" => "services",
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
            Item::Cluster(c) => format!(
                "https://{region}.console.aws.amazon.com/ecs/v2/clusters/{}/services?region={region}",
                c.name
            ),
            Item::Service(s) => {
                let cluster_name = s
                    .cluster_arn
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("default")
                    .to_string();
                format!(
                    "https://{region}.console.aws.amazon.com/ecs/v2/clusters/{cluster_name}/services/{}?region={region}",
                    s.name
                )
            }
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

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

    /// `L` — describe the focused service's task definition, extract
    /// the first awslogs CloudWatch group, then spawn
    /// `mnml-aws-cloudwatch-logs --log-group <group>`. No-op when the
    /// focus is a cluster (no task def at that level) or when the
    /// task def has no awslogs driver (the user might be using splunk,
    /// fluentd, or a sidecar shipper).
    pub fn tail_logs(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Service(svc) = item else {
            self.status = "tail logs is only available on services".into();
            return;
        };
        let Some(td_ref) = svc.task_definition.clone() else {
            self.status = "service has no task definition reference".into();
            return;
        };
        let label = svc.name.clone();
        let region = self.active().spec.region.clone();

        let td = match ecs::describe_task_definition(&td_ref, region.as_deref()) {
            Ok(td) => td,
            Err(e) => {
                self.status = format!("describe-task-definition: {e}");
                return;
            }
        };
        let Some(log_group) = ecs::awslogs_group_for_task_def(&td) else {
            self.status =
                "no awslogs container in task definition — uses a non-CloudWatch driver".into();
            return;
        };

        let mut cmd = std::process::Command::new("mnml-aws-cloudwatch-logs");
        cmd.args(["--log-group", &log_group, "--log-group-name", &label]);
        if let Some(r) = &region {
            cmd.args(["--region", r]);
        }
        match cmd.spawn() {
            Ok(_) => self.status = format!("tailing {log_group}"),
            Err(e) => {
                self.status =
                    format!("spawn failed (install mnml-aws-cloudwatch-logs ≥ v0.2.0): {e}");
            }
        }
    }

    /// `R` — registry handoff. Reads the focused service's task
    /// definition, finds the first container's image reference, and
    /// spawns `mnml-aws-ecr` so the user can hop to "which image is
    /// this service running?" Mirrors the ECS → ECR direction of the
    /// container deploy flow. Sibling auto-scope to the specific
    /// repository is v0.x — today the user navigates manually inside
    /// the spawned mnml-aws-ecr.
    pub fn handoff_ecr(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Service(svc) = item else {
            self.status = "R registry-jump is only available on services".into();
            return;
        };
        let Some(td_ref) = svc.task_definition.clone() else {
            self.status = "service has no task definition reference".into();
            return;
        };
        let label = svc.name.clone();
        let region = self.active().spec.region.clone();

        let td = match ecs::describe_task_definition(&td_ref, region.as_deref()) {
            Ok(td) => td,
            Err(e) => {
                self.status = format!("describe-task-definition: {e}");
                return;
            }
        };
        let Some(image) = td
            .container_definitions
            .iter()
            .find_map(|c| c.image.clone())
        else {
            self.status = "no container image in task definition".into();
            return;
        };
        // ECR image refs look like `<acct>.dkr.ecr.<region>.amazonaws.com/<repo>:<tag>`.
        let repo_label = image
            .split_once('/')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| image.clone());

        let mut cmd = std::process::Command::new("mnml-aws-ecr");
        if let Some(r) = &region {
            cmd.args(["--region", r]);
        }
        match cmd.spawn() {
            Ok(_) => {
                self.status = format!(
                    "launched mnml-aws-ecr — navigate to {repo_label} (service {label}, auto-scope is v0.x)"
                );
            }
            Err(e) => {
                self.status = format!("spawn mnml-aws-ecr failed (install it?): {e}");
            }
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
            kind: "clusters".into(),
            cluster: None,
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_services_without_cluster() {
        let t = Tab {
            name: "bad".into(),
            kind: "services".into(),
            cluster: None,
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }

    #[test]
    fn tab_spec_rejects_unknown_kind() {
        let t = Tab {
            name: "bad".into(),
            kind: "garbage".into(),
            cluster: None,
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }
}
