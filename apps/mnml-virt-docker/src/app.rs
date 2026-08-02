//! App state — per-tab list of docker resources + selection cursor.
//! `docker inspect` for the focused row is loaded lazily, mirroring
//! the AWS-family lazy-attributes pattern. Daemon-not-running is a
//! top-level state, not a per-tab error.

use crate::config::{Config, Tab};
use crate::docker::{self, Container, DaemonState, Image, Network, Volume};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Instant;

/// Job sent from the TUI event loop to the background loader thread.
/// All docker CLI shell-outs run here so a stalled docker daemon
/// can't freeze crossterm input. The two hot paths are:
///
/// - `Refresh` — auto-refresh tick on the active tab. `docker ps` /
///   `docker images` / etc. block on the daemon socket.
/// - `Inspect` — per-keystroke selection move. `docker inspect <id>`
///   is fast on a healthy daemon but seconds-slow if it isn't.
#[derive(Debug)]
enum LoadJob {
    Refresh {
        tab_idx: usize,
        kind: TabKind,
    },
    Inspect {
        tab_idx: usize,
        id: String,
        item_kind: ItemKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    ContainerOrImageOrCompose,
    Volume,
    Network,
}

#[derive(Debug)]
enum LoadResult {
    RefreshItems {
        tab_idx: usize,
        kind: TabKind,
        items: Vec<Item>,
    },
    RefreshFailed {
        tab_idx: usize,
        error: String,
    },
    InspectDetail {
        tab_idx: usize,
        id: String,
        detail: String,
        failed: bool,
    },
}

fn spawn_loader(job_rx: Receiver<LoadJob>, res_tx: Sender<LoadResult>) {
    thread::Builder::new()
        .name("mnml-virt-docker-loader".into())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                match job {
                    LoadJob::Refresh { tab_idx, kind } => {
                        let result: Result<Vec<Item>> = match &kind {
                            TabKind::Containers => docker::list_containers()
                                .map(|cs| cs.into_iter().map(Item::Container).collect()),
                            TabKind::Images => docker::list_images()
                                .map(|is| is.into_iter().map(Item::Image).collect()),
                            TabKind::Volumes => docker::list_volumes()
                                .map(|vs| vs.into_iter().map(Item::Volume).collect()),
                            TabKind::Networks => docker::list_networks()
                                .map(|ns| ns.into_iter().map(Item::Network).collect()),
                            TabKind::Compose { compose_file } => docker::list_compose_services(
                                compose_file.to_string_lossy().as_ref(),
                            )
                            .map(|ss| ss.into_iter().map(Item::ComposeService).collect()),
                        };
                        match result {
                            Ok(items) => {
                                let _ = res_tx.send(LoadResult::RefreshItems {
                                    tab_idx,
                                    kind,
                                    items,
                                });
                            }
                            Err(e) => {
                                let _ = res_tx.send(LoadResult::RefreshFailed {
                                    tab_idx,
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                    LoadJob::Inspect {
                        tab_idx,
                        id,
                        item_kind,
                    } => {
                        let result = match item_kind {
                            ItemKind::ContainerOrImageOrCompose => docker::inspect(&id),
                            ItemKind::Volume => docker::inspect_volume(&id),
                            ItemKind::Network => docker::inspect_network(&id),
                        };
                        let (detail, failed) = match result {
                            Ok(d) => (d, false),
                            Err(e) => (format!("(inspect failed: {e})"), true),
                        };
                        let _ = res_tx.send(LoadResult::InspectDetail {
                            tab_idx,
                            id,
                            detail,
                            failed,
                        });
                    }
                }
            }
        })
        .expect("spawn loader thread");
}

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: TabKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    Containers,
    Images,
    Volumes,
    Networks,
    /// Compose tab — `compose_file` is the resolved absolute path to
    /// `docker-compose.yml` inside the project directory.
    Compose {
        compose_file: PathBuf,
    },
}

impl TabKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TabKind::Containers => "containers",
            TabKind::Images => "images",
            TabKind::Volumes => "volumes",
            TabKind::Networks => "networks",
            TabKind::Compose { .. } => "compose",
        }
    }
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        let kind = match t.kind.as_str() {
            "containers" => TabKind::Containers,
            "images" => TabKind::Images,
            "volumes" => TabKind::Volumes,
            "networks" => TabKind::Networks,
            "compose" => {
                let dir = t.project_path.as_deref().unwrap_or("").trim();
                if dir.is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"compose\" requires `project_path`", t.name);
                }
                let mut p = PathBuf::from(dir);
                // If the user pointed at a file directly, use it as-is.
                // Otherwise (directory or nonexistent path) probe for
                // the conventional compose-file names and fall back
                // to <dir>/docker-compose.yml.
                let looks_like_compose_file = p.extension().is_some()
                    && p.file_name()
                        .map(|n| {
                            let s = n.to_string_lossy();
                            s == "compose.yaml"
                                || s == "compose.yml"
                                || s == "docker-compose.yml"
                                || s == "docker-compose.yaml"
                        })
                        .unwrap_or(false);
                if !looks_like_compose_file {
                    // Prefer compose.yaml (Compose Spec) then compose.yml then docker-compose.yml.
                    let candidates = ["compose.yaml", "compose.yml", "docker-compose.yml"];
                    let mut found = None;
                    for c in candidates {
                        let cand = p.join(c);
                        if cand.exists() {
                            found = Some(cand);
                            break;
                        }
                    }
                    p = found.unwrap_or_else(|| p.join("docker-compose.yml"));
                }
                TabKind::Compose { compose_file: p }
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        };
        Ok(Self { kind })
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Container(Container),
    Image(Image),
    Volume(Volume),
    Network(Network),
    ComposeService(docker::ComposeService),
}

impl Item {
    /// `(name, secondary)` — left column. The secondary is what
    /// trails the bolded primary in the list row.
    pub fn primary_label(&self) -> String {
        match self {
            Item::Container(c) => c.names.clone(),
            Item::Image(i) => i.repo_tag(),
            Item::Volume(v) => v.name.clone(),
            Item::Network(n) => n.name.clone(),
            Item::ComposeService(s) => s.service.clone(),
        }
    }

    pub fn secondary_label(&self) -> String {
        match self {
            Item::Container(c) => {
                let ports = if c.ports.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", c.ports)
                };
                format!("{}  {}{}", c.short_id(), c.image, ports)
            }
            Item::Image(i) => format!("{}  {}  {}", i.short_id(), i.size, i.created_since),
            Item::Volume(v) => format!("{}  {}", v.driver, v.mountpoint),
            Item::Network(n) => format!("{}  {}  {}", n.short_id(), n.driver, n.scope),
            Item::ComposeService(s) => {
                if s.image.is_empty() {
                    s.status.clone()
                } else {
                    format!("{}  {}", s.status, s.image)
                }
            }
        }
    }

    /// State string used for colour cues — same word used by docker
    /// for containers, plus a synthetic value for the other kinds.
    pub fn state(&self) -> &str {
        match self {
            Item::Container(c) => c.state.as_str(),
            Item::Image(_) => "image",
            Item::Volume(_) => "volume",
            Item::Network(_) => "network",
            Item::ComposeService(s) => s.state.as_str(),
        }
    }

    /// The identifier used for `docker inspect`, action commands,
    /// and clipboard yanks.
    pub fn id(&self) -> &str {
        match self {
            Item::Container(c) => &c.id,
            Item::Image(i) => &i.id,
            Item::Volume(v) => &v.name,
            Item::Network(n) => &n.name,
            Item::ComposeService(s) => &s.name,
        }
    }
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
    /// Pretty-printed `docker inspect` output for the focused item,
    /// lazily fetched.
    pub focused_detail: Option<String>,
    /// Track the id we have detail for, so cursor moves trigger a
    /// refetch.
    pub focused_detail_for: Option<String>,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
            focused_detail: None,
            focused_detail_for: None,
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: ItemsTab,
}

/// Pending destructive action — `R` shows the confirm overlay; `y`
/// commits, `n` / Esc cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmPending {
    Container(String),
    Image(String),
    Volume(String),
    Network(String),
}

impl RmPending {
    pub fn description(&self) -> String {
        match self {
            RmPending::Container(id) => format!("remove container {}", short(id)),
            RmPending::Image(id) => format!("remove image {}", short(id)),
            RmPending::Volume(name) => format!("remove volume {name}"),
            RmPending::Network(name) => format!("remove network {name}"),
        }
    }
}

fn short(id: &str) -> &str {
    let cap = id.len().min(12);
    &id[..cap]
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    pub daemon: DaemonState,
    /// `R` confirmation overlay state — `None` = no pending action.
    pub rm_pending: Option<RmPending>,
    /// Queue of commands the UI loop should spawn as pty / external
    /// processes (logs, exec, sibling launches). The crossterm `ui`
    /// loop spawns them directly via `std::process::Command::spawn`.
    /// The blit loop drops them on the floor today (v0.1 standalone
    /// only — blit is wired through but the host-side pty hand-off
    /// is a v0.2 follow-up).
    pub pending_spawns: Vec<Vec<String>>,
    loader_tx: Option<Sender<LoadJob>>,
    loader_rx: Option<Receiver<LoadResult>>,
    /// `id` currently being inspected (or most-recently requested).
    /// Used to coalesce arrow-key spam during selection movement.
    pending_inspect_id: Option<String>,
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t)?;
            tabs.push(TabState {
                name: t.name.clone(),
                data: ItemsTab::empty(),
                spec,
            });
        }
        let daemon = docker::probe_daemon();
        let status = match &daemon {
            DaemonState::Ok(v) => format!("daemon: ok · docker server {v}"),
            DaemonState::Offline => {
                "docker daemon not running — start Docker Desktop, then press r".into()
            }
            DaemonState::CliMissing(e) => format!("docker CLI not found: {e}"),
            DaemonState::Error(e) => format!("docker error: {e}"),
        };
        let (job_tx, job_rx) = mpsc::channel();
        let (res_tx, res_rx) = mpsc::channel();
        spawn_loader(job_rx, res_tx);
        let mut app = App {
            cfg,
            tabs,
            active_tab: 0,
            status,
            daemon,
            rm_pending: None,
            pending_spawns: Vec::new(),
            loader_tx: Some(job_tx),
            loader_rx: Some(res_rx),
            pending_inspect_id: None,
        };
        if matches!(app.daemon, DaemonState::Ok(_)) {
            app.refresh_active();
            app.ensure_focused_loaded();
        }
        Ok(app)
    }

    /// Drain results from the loader. Call from `tick` BEFORE rendering.
    /// Inspect results land in the right tab via `tab_idx`. A late
    /// refresh result for a now-inactive tab still applies items (the
    /// renderer just shows them next time the user swaps back).
    pub fn drain_load_results(&mut self) -> bool {
        let Some(rx) = self.loader_rx.as_ref() else {
            return false;
        };
        let mut changed = false;
        loop {
            let msg = match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.status = "loader thread died".to_string();
                    break;
                }
            };
            match msg {
                LoadResult::RefreshItems {
                    tab_idx,
                    kind,
                    items,
                } => {
                    let Some(t) = self.tabs.get_mut(tab_idx) else {
                        continue;
                    };
                    let count = items.len();
                    let prev_id = t
                        .data
                        .items
                        .get(t.data.selected)
                        .map(|i| i.id().to_string());
                    t.data.loading = false;
                    t.data.items = items;
                    t.data.selected = t.data.selected.min(count.saturating_sub(1));
                    let new_id = t
                        .data
                        .items
                        .get(t.data.selected)
                        .map(|i| i.id().to_string());
                    if prev_id != new_id {
                        t.data.focused_detail = None;
                        t.data.focused_detail_for = None;
                    }
                    t.data.last_loaded = Some(Instant::now());
                    t.data.last_error = None;
                    if tab_idx == self.active_tab {
                        let name = t.name.clone();
                        self.status =
                            format!("{name}: {count} {kind_label}", kind_label = kind.as_str());
                    }
                    changed = true;
                }
                LoadResult::RefreshFailed { tab_idx, error } => {
                    let Some(t) = self.tabs.get_mut(tab_idx) else {
                        continue;
                    };
                    t.data.loading = false;
                    if docker::is_daemon_offline(&error) {
                        self.daemon = DaemonState::Offline;
                        self.status =
                            "docker daemon not running — start Docker Desktop, then press r".into();
                    } else {
                        t.data.last_error = Some(error.clone());
                        if tab_idx == self.active_tab {
                            self.status = format!("error: {error}");
                        }
                    }
                    changed = true;
                }
                LoadResult::InspectDetail {
                    tab_idx,
                    id,
                    detail,
                    failed,
                } => {
                    let _ = failed;
                    if self.pending_inspect_id.as_ref() == Some(&id) {
                        self.pending_inspect_id = None;
                    }
                    let Some(t) = self.tabs.get_mut(tab_idx) else {
                        continue;
                    };
                    t.data.focused_detail = Some(detail);
                    t.data.focused_detail_for = Some(id);
                    changed = true;
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

    pub fn daemon_online(&self) -> bool {
        matches!(self.daemon, DaemonState::Ok(_))
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            if !self.daemon_online() {
                return;
            }
            if self.tabs[idx].data.items.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
            self.ensure_focused_loaded();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        {
            let tab = self.active_mut();
            if tab.data.items.is_empty() {
                return;
            }
            let n = tab.data.items.len() as isize;
            let cur = tab.data.selected as isize;
            let next = (cur + delta).clamp(0, n - 1);
            tab.data.selected = next as usize;
        }
        self.ensure_focused_loaded();
    }

    pub fn refresh_active(&mut self) {
        // Re-probe the daemon if we're offline — `r` is the documented
        // "I just started Docker, try again" key. `docker version` is
        // a fast shell-out so keep it on the main thread.
        if !self.daemon_online() {
            let new_state = docker::probe_daemon();
            self.daemon = new_state;
            match &self.daemon {
                DaemonState::Ok(v) => {
                    self.status = format!("daemon: ok · docker server {v}");
                }
                DaemonState::Offline => {
                    self.status =
                        "docker daemon still offline — start Docker Desktop, then press r".into();
                    return;
                }
                DaemonState::CliMissing(e) => {
                    self.status = format!("docker CLI not found: {e}");
                    return;
                }
                DaemonState::Error(e) => {
                    self.status = format!("docker error: {e}");
                    return;
                }
            }
        }

        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        if self.tabs[idx].data.loading {
            return;
        }
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;
        if let Some(tx) = self.loader_tx.as_ref() {
            if let Err(e) = tx.send(LoadJob::Refresh {
                tab_idx: idx,
                kind: spec.kind,
            }) {
                self.tabs[idx].data.loading = false;
                self.status = format!("loader send failed: {e}");
            }
        } else {
            self.tabs[idx].data.loading = false;
        }
    }

    pub fn ensure_focused_loaded(&mut self) {
        if !self.daemon_online() {
            return;
        }
        let idx = self.active_tab;
        let sel = self.tabs[idx].data.selected;
        let Some(item) = self.tabs[idx].data.items.get(sel) else {
            return;
        };
        let id = item.id().to_string();
        if self.tabs[idx].data.focused_detail_for.as_deref() == Some(id.as_str())
            && self.tabs[idx].data.focused_detail.is_some()
        {
            return;
        }
        // Coalesce: already inspecting THIS id ⇒ don't re-queue.
        if self.pending_inspect_id.as_ref() == Some(&id) {
            return;
        }
        let item_kind = match item {
            Item::Container(_) | Item::Image(_) | Item::ComposeService(_) => {
                ItemKind::ContainerOrImageOrCompose
            }
            Item::Volume(_) => ItemKind::Volume,
            Item::Network(_) => ItemKind::Network,
        };
        self.pending_inspect_id = Some(id.clone());
        if let Some(tx) = self.loader_tx.as_ref() {
            let _ = tx.send(LoadJob::Inspect {
                tab_idx: idx,
                id,
                item_kind,
            });
        }
    }

    pub fn tick(&mut self) -> bool {
        let loader_changed = self.drain_load_results();
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 || !self.daemon_online() {
            return loader_changed;
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
            loader_changed
        }
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    /// `o` — open Docker Desktop (macOS) or noop with toast (Linux/Win).
    pub fn open_docker_desktop(&mut self) {
        if cfg!(target_os = "macos") {
            match std::process::Command::new("open")
                .args(["-a", "Docker Desktop"])
                .spawn()
            {
                Ok(_) => self.status = "opened Docker Desktop".into(),
                Err(e) => self.status = format!("open Docker Desktop failed: {e}"),
            }
        } else {
            self.status = "Docker Desktop launch only supported on macOS".into();
        }
    }

    /// `y` — yank the focused item's full ID/name.
    pub fn yank_id(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let id = item.id().to_string();
        if id.is_empty() {
            self.status = "no ID to yank".into();
            return;
        }
        match crate::clipboard::copy(&id) {
            Ok(()) => self.status = format!("copied: {id}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `l` — tail logs for the focused container in a follow loop.
    /// Containers only; other tabs get a toast.
    pub fn tail_logs(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Container(c) = item else {
            self.status = "logs: only available on containers".into();
            return;
        };
        let id = c.id.clone();
        let label = c.names.clone();
        self.pending_spawns.push(vec![
            "docker".into(),
            "logs".into(),
            "-f".into(),
            id.clone(),
        ]);
        self.status = format!("tailing logs for {label}…");
    }

    /// `e` — exec a shell into the focused running container. Tries
    /// `/bin/bash` first then `/bin/sh`. Other tabs / non-running
    /// containers: toast.
    pub fn exec_shell(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Container(c) = item else {
            self.status = "exec: only available on containers".into();
            return;
        };
        if !c.is_running() {
            self.status = format!("exec: {} is not running", c.names);
            return;
        }
        let id = c.id.clone();
        let label = c.names.clone();
        // We can't `try /bin/bash, fall back to /bin/sh` from a
        // single spawn — punt the decision to the shell. `which`
        // chain works in either busybox or coreutils.
        let chain = "if [ -x /bin/bash ]; then exec /bin/bash; else exec /bin/sh; fi";
        self.pending_spawns.push(vec![
            "docker".into(),
            "exec".into(),
            "-it".into(),
            id,
            "/bin/sh".into(),
            "-c".into(),
            chain.into(),
        ]);
        self.status = format!("exec into {label}…");
    }

    /// `s` — stop focused container. `S` — start. No confirmation —
    /// both are reversible.
    pub fn stop_or_start(&mut self, start: bool) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Container(c) = item else {
            self.status = "stop/start: only available on containers".into();
            return;
        };
        let id = c.id.clone();
        let label = c.names.clone();
        let res = if start {
            docker::start_container(&id)
        } else {
            docker::stop_container(&id)
        };
        match res {
            Ok(()) => {
                self.status = if start {
                    format!("started {label}")
                } else {
                    format!("stopped {label}")
                };
                self.refresh_active();
            }
            Err(e) => self.status = format!("{e}"),
        }
    }

    /// `R` — show the rm-confirmation overlay for the focused item.
    /// Idempotent (calling twice keeps the same pending action).
    pub fn enter_rm_confirm(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let pending = match item {
            Item::Container(c) => RmPending::Container(c.id.clone()),
            Item::Image(i) => RmPending::Image(i.id.clone()),
            Item::Volume(v) => RmPending::Volume(v.name.clone()),
            Item::Network(n) => RmPending::Network(n.name.clone()),
            Item::ComposeService(_) => {
                self.status =
                    "rm: not supported for compose services — `docker compose down` instead".into();
                return;
            }
        };
        self.status = format!(
            "{} — y to confirm, n / Esc to cancel",
            pending.description()
        );
        self.rm_pending = Some(pending);
    }

    /// Confirm pending rm.
    pub fn confirm_rm(&mut self) {
        let Some(pending) = self.rm_pending.take() else {
            return;
        };
        let res = match &pending {
            RmPending::Container(id) => docker::rm_container(id),
            RmPending::Image(id) => docker::rmi_image(id),
            RmPending::Volume(name) => docker::rm_volume(name),
            RmPending::Network(name) => docker::rm_network(name),
        };
        match res {
            Ok(()) => {
                self.status = format!("done: {}", pending.description());
                self.refresh_active();
            }
            Err(e) => self.status = format!("{e}"),
        }
    }

    /// Cancel pending rm (Esc or `n`).
    pub fn cancel_rm(&mut self) {
        if self.rm_pending.take().is_some() {
            self.status = "rm cancelled".into();
        }
    }

    /// `L` — cross-sibling jump: if focused image is an ECR URL,
    /// spawn `mnml-aws-ecr --region <region>`. Otherwise toast.
    /// Only available on the images tab.
    pub fn handoff_ecr(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let image = match item {
            Item::Image(i) => i.repo_tag(),
            Item::Container(c) => c.image.clone(),
            _ => {
                self.status = "L jump: only available on images / containers".into();
                return;
            }
        };
        let Some((_acct, region)) = docker::parse_ecr_url(&image) else {
            self.status = format!("not an ECR image: {image}");
            return;
        };
        match std::process::Command::new("mnml-aws-ecr")
            .args(["--region", &region])
            .spawn()
        {
            Ok(_) => self.status = format!("launched mnml-aws-ecr ({region})"),
            Err(e) => self.status = format!("spawn mnml-aws-ecr failed (install it?): {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;
    use crate::docker::{Container, Image};

    fn cfg_with(tabs: Vec<Tab>) -> Config {
        Config {
            refresh_interval_secs: 0, // disable tick auto-refresh
            tabs,
        }
    }

    #[test]
    fn tab_spec_resolve_known_kinds() {
        for kind in ["containers", "images", "volumes", "networks"] {
            let t = Tab {
                name: kind.into(),
                kind: kind.into(),
                project_path: None,
            };
            let spec = TabSpec::resolve(&t).unwrap();
            assert_eq!(spec.kind.as_str(), kind);
        }
    }

    #[test]
    fn tab_spec_compose_resolves_compose_file() {
        let t = Tab {
            name: "myapp".into(),
            kind: "compose".into(),
            project_path: Some("/nonexistent/dir".into()),
        };
        let spec = TabSpec::resolve(&t).unwrap();
        if let TabKind::Compose { compose_file } = spec.kind {
            // Falls back to <dir>/docker-compose.yml when the dir
            // doesn't exist (we don't probe it then).
            assert!(
                compose_file
                    .to_string_lossy()
                    .ends_with("docker-compose.yml")
            );
        } else {
            panic!("expected compose kind");
        }
    }

    #[test]
    fn rm_state_machine_pending_then_cancel() {
        let cfg = cfg_with(vec![Tab {
            name: "containers".into(),
            kind: "containers".into(),
            project_path: None,
        }]);
        // Construct an App without going through ::new (which would
        // probe the docker daemon) — drop into raw state.
        let mut app = App {
            cfg,
            tabs: vec![TabState {
                name: "containers".into(),
                spec: TabSpec {
                    kind: TabKind::Containers,
                },
                data: ItemsTab::empty(),
            }],
            active_tab: 0,
            status: String::new(),
            daemon: DaemonState::Ok("test".into()),
            rm_pending: None,
            pending_spawns: Vec::new(),
            loader_tx: None,
            loader_rx: None,
            pending_inspect_id: None,
        };
        app.tabs[0].data.items.push(Item::Container(Container {
            id: "abc123def456".into(),
            image: "redis:7".into(),
            names: "redis".into(),
            status: "Up".into(),
            state: "running".into(),
            ports: String::new(),
            running_for: String::new(),
            command: String::new(),
            created_at: String::new(),
        }));
        assert!(app.rm_pending.is_none());
        app.enter_rm_confirm();
        assert_eq!(
            app.rm_pending,
            Some(RmPending::Container("abc123def456".into()))
        );
        app.cancel_rm();
        assert!(app.rm_pending.is_none());
        assert!(app.status.contains("cancelled"));
    }

    #[test]
    fn rm_state_machine_compose_service_rejected() {
        let cfg = cfg_with(vec![Tab {
            name: "compose".into(),
            kind: "containers".into(),
            project_path: None,
        }]);
        let mut app = App {
            cfg,
            tabs: vec![TabState {
                name: "compose".into(),
                spec: TabSpec {
                    kind: TabKind::Containers,
                },
                data: ItemsTab::empty(),
            }],
            active_tab: 0,
            status: String::new(),
            daemon: DaemonState::Ok("test".into()),
            rm_pending: None,
            pending_spawns: Vec::new(),
            loader_tx: None,
            loader_rx: None,
            pending_inspect_id: None,
        };
        app.tabs[0]
            .data
            .items
            .push(Item::ComposeService(docker::ComposeService {
                name: "web-1".into(),
                service: "web".into(),
                state: "running".into(),
                status: "Up".into(),
                image: "redis:7".into(),
                project: "myapp".into(),
            }));
        app.enter_rm_confirm();
        assert!(app.rm_pending.is_none());
        assert!(app.status.contains("compose"));
    }

    #[test]
    fn item_primary_and_secondary_labels() {
        let i = Item::Image(Image {
            id: "sha256:abcdef1234567890".into(),
            repository: "redis".into(),
            tag: "7".into(),
            size: "110MB".into(),
            created_since: "3 weeks ago".into(),
            created_at: String::new(),
            digest: String::new(),
        });
        assert_eq!(i.primary_label(), "redis:7");
        assert!(i.secondary_label().contains("110MB"));
    }
}
