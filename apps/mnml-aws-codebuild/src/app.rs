//! App state — read-only browser over CodeBuild projects.
//!
//! Same shape as the sibling EventBridge Schedules browser: a
//! single scrollable list of projects, each row collapses by
//! default and expands to show the fields the user cares about.

use crate::codebuild::{self, CodeBuildRecord, ProjectDetail};
use crate::config::Config;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

pub type PendingProjects = Receiver<Result<Vec<String>, String>>;
pub type PendingDetails = Receiver<Result<Vec<ProjectDetail>, String>>;
pub type BuildsStream = Receiver<(String, Result<Vec<CodeBuildRecord>, String>)>;
pub type SingleRefetch = (String, Receiver<Result<Vec<CodeBuildRecord>, String>>);

pub struct App {
    pub cfg: Config,
    /// Sorted list of project names — the row set.
    pub projects: Vec<String>,
    pub selected: usize,
    /// Full detail per project, keyed by name.
    pub detail_cache: HashMap<String, ProjectDetail>,
    /// Recent-builds per project.
    pub builds_cache: HashMap<String, Vec<CodeBuildRecord>>,
    /// Rows the user has expanded.
    pub expanded: HashSet<String>,
    /// Hit-test rects captured by the renderer each frame.
    pub row_hits: Vec<(usize, u16, u16)>,
    pub scroll_offset: u16,
    pub status: String,
    pub last_error: Option<String>,
    pub pending_projects: Option<PendingProjects>,
    pub pending_details: Option<PendingDetails>,
    pub pending_builds: Option<BuildsStream>,
    /// One-shot re-fetch of a single project's recent builds —
    /// used by the post-`start-build` refresh. Kept SEPARATE from
    /// `pending_builds` so it doesn't clobber the initial full-list
    /// prefetch (which was the CRITICAL bug reported 2026-07-22).
    /// Value: (project_name, one-shot receiver).
    pub pending_single_refetch: Option<SingleRefetch>,
    /// In-flight `start-build` request (name being kicked off,
    /// receiver for the aws call result).
    pub pending_start: Option<(String, std::sync::mpsc::Receiver<Result<String, String>>)>,
    pub should_quit: bool,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        let mut app = App {
            cfg,
            projects: Vec::new(),
            selected: 0,
            detail_cache: HashMap::new(),
            builds_cache: HashMap::new(),
            expanded: HashSet::new(),
            row_hits: Vec::new(),
            scroll_offset: 0,
            status: "loading projects…".to_string(),
            last_error: None,
            pending_projects: None,
            pending_details: None,
            pending_builds: None,
            pending_single_refetch: None,
            pending_start: None,
            should_quit: false,
        };
        app.refresh();
        app
    }

    /// Kick off `list-projects` + wire up the details/builds
    /// prefetch pipeline. Clears caches so `r` gives a clean
    /// refresh.
    pub fn refresh(&mut self) {
        self.detail_cache.clear();
        self.builds_cache.clear();
        // Drop in-flight streams so their late results don't land
        // in the freshly-cleared cache (2026-07-22 tester finding).
        // Background threads will fail-send silently + exit.
        self.pending_details = None;
        self.pending_builds = None;
        self.pending_single_refetch = None;
        let region = self.cfg.region.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(codebuild::list_projects(region.as_deref()));
        });
        self.pending_projects = Some(rx);
        self.status = "refreshing…".to_string();
    }

    fn start_details_and_builds(&mut self) {
        // batch-get-projects on the full name list (single call).
        let names = self.projects.clone();
        let region = self.cfg.region.clone();
        let (dtx, drx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = dtx.send(codebuild::batch_get_projects(&names, region.as_deref()));
        });
        self.pending_details = Some(drx);
        // Recent-builds — one call per project, streamed.
        let names = self.projects.clone();
        let region = self.cfg.region.clone();
        let limit = self.cfg.recent_builds;
        let (btx, brx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for name in names {
                let r = codebuild::recent_builds(&name, limit, region.as_deref());
                if btx.send((name, r)).is_err() {
                    break;
                }
            }
        });
        self.pending_builds = Some(brx);
    }

    /// Fire `aws codebuild start-build` on the selected project.
    /// Refuses if a start is already in flight (single-flight; the
    /// aws CLI itself is idempotent per invocation but the UI
    /// shouldn't fire twice on double-tap).
    pub fn start_build(&mut self) {
        if self.pending_start.is_some() {
            self.status = "start-build already in flight".to_string();
            return;
        }
        let Some(name) = self.projects.get(self.selected).cloned() else {
            self.status = "no project under cursor".to_string();
            return;
        };
        let region = self.cfg.region.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let name_t = name.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::codebuild::start_build(&name_t, region.as_deref()));
        });
        self.pending_start = Some((name.clone(), rx));
        self.status = format!("starting build for {name}…");
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.projects.len();
        if n == 0 {
            return;
        }
        // Clamp (not wrap) — was `rem_euclid` which had PageDown at
        // the bottom teleport to the top. Vim canonical.
        let s = (self.selected as isize + delta).clamp(0, n as isize - 1) as usize;
        self.selected = s;
    }

    pub fn toggle_expand_at(&mut self, idx: usize) {
        let Some(name) = self.projects.get(idx).cloned() else {
            return;
        };
        if self.expanded.contains(&name) {
            self.expanded.remove(&name);
        } else {
            self.expanded.insert(name);
        }
    }

    pub fn expand_selected(&mut self) {
        if let Some(name) = self.projects.get(self.selected).cloned() {
            self.expanded.insert(name);
        }
    }

    pub fn collapse_selected(&mut self) {
        if let Some(name) = self.projects.get(self.selected) {
            self.expanded.remove(name);
        }
    }

    pub fn poll_background(&mut self) -> bool {
        let mut any = false;
        if let Some(rx) = &self.pending_projects {
            match rx.try_recv() {
                Ok(Ok(list)) => {
                    // Apply the config allow-list here (case-sensitive
                    // exact match against the raw project name).
                    let filter = &self.cfg.projects;
                    self.projects = if filter.is_empty() {
                        list
                    } else {
                        list.into_iter()
                            .filter(|n| filter.iter().any(|f| f == n))
                            .collect()
                    };
                    self.selected = self.selected.min(self.projects.len().saturating_sub(1));
                    self.status = format!("{} projects", self.projects.len());
                    self.pending_projects = None;
                    self.start_details_and_builds();
                    any = true;
                }
                Ok(Err(e)) => {
                    self.last_error = Some(e.clone());
                    self.status = format!("list error: {e}");
                    self.pending_projects = None;
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_projects = None;
                }
            }
        }
        if let Some(rx) = &self.pending_details {
            match rx.try_recv() {
                Ok(Ok(details)) => {
                    for d in details {
                        self.detail_cache.insert(d.name.clone(), d);
                    }
                    self.pending_details = None;
                    any = true;
                }
                Ok(Err(e)) => {
                    self.last_error = Some(e.clone());
                    self.status = format!("detail error: {e}");
                    self.pending_details = None;
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_details = None;
                }
            }
        }
        // Drain the builds stream — collect then apply.
        let mut batch: Vec<(String, Vec<CodeBuildRecord>)> = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &self.pending_builds {
            loop {
                match rx.try_recv() {
                    Ok((name, Ok(builds))) => batch.push((name, builds)),
                    Ok((_, Err(_))) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.pending_builds = None;
        }
        if !batch.is_empty() {
            any = true;
            for (name, builds) in batch {
                self.builds_cache.insert(name, builds);
            }
        }
        // Drain the single-project re-fetch triggered by start-build.
        if let Some((_, rx)) = &self.pending_single_refetch {
            match rx.try_recv() {
                Ok(Ok(builds)) => {
                    if let Some((name, _)) = self.pending_single_refetch.take() {
                        self.builds_cache.insert(name, builds);
                    }
                    any = true;
                }
                Ok(Err(e)) => {
                    if let Some((name, _)) = self.pending_single_refetch.take() {
                        self.status = format!("refetch {name}: {e}");
                    }
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_single_refetch = None;
                }
            }
        }
        // Drain start-build.
        if let Some((_, rx)) = &self.pending_start {
            match rx.try_recv() {
                Ok(Ok(build_id)) => {
                    let name = self
                        .pending_start
                        .take()
                        .map(|(n, _)| n)
                        .unwrap_or_default();
                    self.status = format!("started {name} — build id {build_id}");
                    // The new build won't appear until
                    // list-builds-for-project sees it; refire
                    // recent-builds for just this project through
                    // `pending_single_refetch` (a SEPARATE slot from
                    // `pending_builds` so the initial full-list
                    // prefetch stream isn't dropped mid-stream —
                    // 2026-07-22 CRITICAL regression fix).
                    let region = self.cfg.region.clone();
                    let limit = self.cfg.recent_builds;
                    let name_t = name.clone();
                    let (tx, brx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let r = crate::codebuild::recent_builds(&name_t, limit, region.as_deref());
                        let _ = tx.send(r);
                    });
                    self.pending_single_refetch = Some((name.clone(), brx));
                    any = true;
                }
                Ok(Err(e)) => {
                    let name = self
                        .pending_start
                        .take()
                        .map(|(n, _)| n)
                        .unwrap_or_default();
                    self.status = format!("start-build {name} failed: {e}");
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_start = None;
                }
            }
        }
        any
    }
}
