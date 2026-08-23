//! App state — bucket tabs, breadcrumb stack, selection. The S3
//! calls happen on a worker thread; the App polls a channel each
//! tick to drain results.

use crate::config::{Bucket, Config};
use crate::picker::FilePicker;
use crate::s3::{self, Entry};
use crate::upload::{self, UploadEvent};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Instant;

/// Max simultaneous uploads. Above this, tasks wait in `Queued`.
/// Keeps a 100-file batch from hammering the AWS TPS bucket (default
/// 3500 PUT/s per prefix) and matches `aws s3 cp --recursive`'s
/// default.
const UPLOAD_CONCURRENCY: usize = 4;

#[derive(Debug)]
pub struct TabState {
    pub name: String,
    pub bucket: String,
    pub region: Option<String>,
    /// Current prefix the listing is anchored at. Always ends in
    /// `/` when non-empty (so `list-objects-v2 --delimiter /` does
    /// the right thing).
    pub prefix: String,
    /// Stack of prefixes for Backspace / `h` to pop. Top of the
    /// stack is the immediate parent.
    pub prefix_stack: Vec<String>,
    pub items: Vec<Entry>,
    pub selected: usize,
    pub last_error: Option<String>,
    pub loading: bool,
    pub pending: Option<Receiver<S3Event>>,
}

#[derive(Debug, Clone)]
pub enum S3Event {
    Listed(Vec<Entry>),
    Failed(String),
}

pub struct App {
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// Pending confirmation prompt — set when the user presses
    /// `d` to delete. The UI surfaces "delete <key>? y/N" and the
    /// next key press resolves it.
    pub pending_confirm: Option<PendingConfirm>,
    /// The upload overlay — a two-phase modal: pick files, then watch
    /// them upload with per-file progress.
    pub upload_overlay: Option<UploadOverlay>,
    /// Auto-incrementing id for new upload tasks. Used as a stable key
    /// so the UI can re-associate progress ticks even when tasks
    /// finish out of order.
    upload_next_id: u64,
}

/// The upload overlay is either the file picker (before firing) or a
/// progress panel (after firing). Kept separate so the picker doesn't
/// need to know about progress and vice versa.
#[derive(Debug)]
pub enum UploadOverlay {
    Pick(FilePicker),
    Progress(UploadProgress),
}

#[derive(Debug)]
pub struct UploadProgress {
    /// Bucket + prefix uploads are heading to (locked at fire time).
    pub bucket: String,
    pub prefix: String,
    pub tasks: Vec<UploadTask>,
    /// True after every task finishes (successfully or otherwise), so
    /// the UI can flip to a "close with any key" state.
    pub all_done: bool,
}

#[derive(Debug)]
pub struct UploadTask {
    /// Stable id — reserved for future selective retry / cancel by
    /// task; the UI keys off Vec position today.
    #[allow(dead_code)]
    pub id: u64,
    pub local: PathBuf,
    pub name: String,
    pub key: String,
    pub total: u64,
    pub done: u64,
    pub rate_bps: u64,
    pub state: UploadState,
    /// The receiver, present while the task is Running. `Queued` tasks
    /// have None here until we promote them.
    pending: Option<Receiver<UploadEvent>>,
    /// When the task went from Queued → Running.
    #[allow(dead_code)]
    started_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum UploadState {
    Queued,
    Running,
    Done,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum PendingConfirm {
    Delete {
        bucket: String,
        key: String,
        region: Option<String>,
    },
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs: Vec<TabState> = Vec::with_capacity(cfg.buckets.len());
        for b in &cfg.buckets {
            tabs.push(tab_from_config(b));
        }
        let mut app = App {
            tabs,
            active_tab: 0,
            status: String::new(),
            pending_confirm: None,
            upload_overlay: None,
            upload_next_id: 1,
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
        let (bucket, prefix, region, name) = {
            let t = &self.tabs[idx];
            (
                t.bucket.clone(),
                t.prefix.clone(),
                t.region.clone(),
                t.name.clone(),
            )
        };
        self.status = format!("listing s3://{bucket}/{prefix}…");
        let (tx, rx) = channel();
        thread::spawn(move || {
            let result = s3::list_prefix(&bucket, &prefix, region.as_deref());
            let _ = match result {
                Ok(items) => tx.send(S3Event::Listed(items)),
                Err(e) => tx.send(S3Event::Failed(e.to_string())),
            };
        });
        let t = &mut self.tabs[idx];
        t.loading = true;
        t.last_error = None;
        t.pending = Some(rx);
        let _ = name;
    }

    /// Drain background channels — call from the main loop each
    /// tick. Returns true if anything changed (redraw).
    pub fn drain(&mut self) -> bool {
        let mut any = self.drain_listings();
        any |= self.drain_uploads();
        any |= self.promote_queued_uploads();
        any
    }

    fn drain_listings(&mut self) -> bool {
        let mut any = false;
        for tab in self.tabs.iter_mut() {
            let Some(rx) = tab.pending.take() else {
                continue;
            };
            let mut done = false;
            loop {
                match rx.try_recv() {
                    Ok(S3Event::Listed(items)) => {
                        any = true;
                        let n = items.len();
                        tab.items = items;
                        tab.loading = false;
                        tab.last_error = None;
                        if tab.selected >= tab.items.len() {
                            tab.selected = tab.items.len().saturating_sub(1);
                        }
                        done = true;
                        self.status = format!(
                            "{} · s3://{}/{} · {n} entries",
                            tab.name, tab.bucket, tab.prefix
                        );
                    }
                    Ok(S3Event::Failed(e)) => {
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

    fn drain_uploads(&mut self) -> bool {
        // Scope the mutable borrow on `self.upload_overlay` so we can
        // call `self.refresh_active()` afterwards without a conflict.
        let (any, just_finished, succ, fail, same_bucket) = {
            let Some(UploadOverlay::Progress(progress)) = self.upload_overlay.as_mut() else {
                return false;
            };
            let mut any = false;
            for task in progress.tasks.iter_mut() {
                let Some(rx) = task.pending.take() else {
                    continue;
                };
                let mut keep = true;
                loop {
                    match rx.try_recv() {
                        Ok(UploadEvent::Progress {
                            done,
                            total,
                            rate_bps,
                        }) => {
                            any = true;
                            task.done = done;
                            if total > 0 {
                                task.total = total;
                            }
                            task.rate_bps = rate_bps;
                        }
                        Ok(UploadEvent::Completed) => {
                            any = true;
                            task.state = UploadState::Done;
                            task.done = task.total.max(task.done);
                            task.rate_bps = 0;
                            keep = false;
                        }
                        Ok(UploadEvent::Failed(msg)) => {
                            any = true;
                            task.state = UploadState::Failed(msg);
                            task.rate_bps = 0;
                            keep = false;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            // Channel closed but no terminal event —
                            // treat as failure so the row doesn't
                            // spin forever.
                            if matches!(task.state, UploadState::Running) {
                                task.state = UploadState::Failed("dropped".into());
                            }
                            keep = false;
                            break;
                        }
                    }
                }
                if keep {
                    task.pending = Some(rx);
                }
            }
            let all_done = progress
                .tasks
                .iter()
                .all(|t| matches!(t.state, UploadState::Done | UploadState::Failed(_)));
            let just_finished = all_done && !progress.all_done;
            if just_finished {
                progress.all_done = true;
                any = true;
            }
            let succ = progress
                .tasks
                .iter()
                .filter(|t| matches!(t.state, UploadState::Done))
                .count();
            let fail = progress.tasks.len() - succ;
            let idx = self.active_tab;
            let same_bucket = self.tabs[idx].bucket == progress.bucket
                && self.tabs[idx].prefix == progress.prefix;
            (any, just_finished, succ, fail, same_bucket)
        };
        if just_finished {
            if same_bucket {
                self.refresh_active();
            }
            self.status = if fail == 0 {
                format!("uploaded {succ} file(s)")
            } else {
                format!("uploaded {succ}, {fail} failed")
            };
        }
        any
    }

    /// Move Queued tasks to Running until we hit the concurrency cap.
    fn promote_queued_uploads(&mut self) -> bool {
        let Some(UploadOverlay::Progress(progress)) = self.upload_overlay.as_mut() else {
            return false;
        };
        let region = self.tabs[self.active_tab].region.clone();
        let bucket = progress.bucket.clone();
        let running = progress
            .tasks
            .iter()
            .filter(|t| matches!(t.state, UploadState::Running))
            .count();
        let mut any = false;
        let mut slots = UPLOAD_CONCURRENCY.saturating_sub(running);
        for task in progress.tasks.iter_mut() {
            if slots == 0 {
                break;
            }
            if !matches!(task.state, UploadState::Queued) {
                continue;
            }
            let rx = upload::spawn_upload(
                task.local.clone(),
                bucket.clone(),
                task.key.clone(),
                region.clone(),
                task.total,
            );
            task.pending = Some(rx);
            task.state = UploadState::Running;
            task.started_at = Some(Instant::now());
            slots -= 1;
            any = true;
        }
        any
    }

    /// `Enter` on a prefix → drill in. On a file → download to the
    /// cache and toast the local path.
    pub fn enter_focused(&mut self) {
        let Some(entry) = self.focused_entry().cloned() else {
            return;
        };
        match entry {
            Entry::Prefix(p) => {
                let tab = self.active_mut();
                tab.prefix_stack.push(tab.prefix.clone());
                tab.prefix = format!("{}{}", tab.prefix, p.name);
                tab.selected = 0;
                tab.items.clear();
                self.refresh_active();
            }
            Entry::Object(o) => {
                let idx = self.active_tab;
                let (bucket, region) = {
                    let t = &self.tabs[idx];
                    (t.bucket.clone(), t.region.clone())
                };
                let dest = cache_path_for(&bucket, &o.key);
                self.status = format!("downloading {}…", o.key);
                match s3::download(&bucket, &o.key, &dest, region.as_deref()) {
                    Ok(path) => {
                        let path_str = path.to_string_lossy().to_string();
                        self.status = format!("downloaded → {path_str}");
                    }
                    Err(e) => self.status = format!("download failed: {e}"),
                }
            }
        }
    }

    /// `u` — open the file picker rooted at the user's cwd. Space
    /// toggles selection; Enter with selections (or on a file with no
    /// selections) fires the uploads.
    pub fn start_upload_prompt(&mut self) {
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));
        self.upload_overlay = Some(UploadOverlay::Pick(FilePicker::new(cwd)));
        let (bucket, prefix) = {
            let tab = self.active();
            (tab.bucket.clone(), tab.prefix.clone())
        };
        self.status = format!(
            "upload picker · target s3://{bucket}/{prefix} · Space toggle · Enter upload · Esc cancel"
        );
    }

    /// Cancel the upload overlay. If uploads are in flight, they keep
    /// running (the worker threads are detached); we just hide the UI.
    pub fn upload_cancel(&mut self) {
        if self.upload_overlay.take().is_some() {
            self.status = "upload cancelled".into();
        }
    }

    /// Access the picker for the keys layer to dispatch into.
    pub fn picker_mut(&mut self) -> Option<&mut FilePicker> {
        match self.upload_overlay.as_mut() {
            Some(UploadOverlay::Pick(p)) => Some(p),
            _ => None,
        }
    }

    /// True iff the overlay is showing progress (not the picker).
    pub fn is_upload_running(&self) -> bool {
        matches!(self.upload_overlay, Some(UploadOverlay::Progress(_)))
    }

    /// Called by the picker's Enter action when it decided to fire —
    /// flip the overlay from Pick to Progress and enqueue tasks.
    pub fn upload_fire(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            self.status = "upload cancelled (no files)".into();
            self.upload_overlay = None;
            return;
        }
        let idx = self.active_tab;
        let (bucket, prefix) = {
            let t = &self.tabs[idx];
            (t.bucket.clone(), t.prefix.clone())
        };
        let mut tasks = Vec::with_capacity(paths.len());
        for local in paths {
            let total = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
            let name = local
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("upload")
                .to_string();
            let key = format!("{prefix}{name}");
            let id = self.upload_next_id;
            self.upload_next_id += 1;
            tasks.push(UploadTask {
                id,
                local,
                name,
                key,
                total,
                done: 0,
                rate_bps: 0,
                state: UploadState::Queued,
                pending: None,
                started_at: None,
            });
        }
        let count = tasks.len();
        self.upload_overlay = Some(UploadOverlay::Progress(UploadProgress {
            bucket: bucket.clone(),
            prefix: prefix.clone(),
            tasks,
            all_done: false,
        }));
        self.status = format!("uploading {count} file(s) to s3://{bucket}/{prefix}…");
    }

    /// Backspace / `h` — go up one prefix level.
    pub fn pop_prefix(&mut self) {
        let tab = self.active_mut();
        let Some(prev) = tab.prefix_stack.pop() else {
            return;
        };
        tab.prefix = prev;
        tab.selected = 0;
        tab.items.clear();
        self.refresh_active();
    }

    /// `y` — yank `s3://bucket/key` URI for the focused entry.
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

    /// `Y` — yank a presigned URL (5-min TTL) for the focused
    /// object. No-op on prefix rows.
    pub fn yank_presigned(&mut self) {
        let Some(entry) = self.focused_entry().cloned() else {
            return;
        };
        let Entry::Object(o) = entry else {
            self.status = "presign only applies to files".into();
            return;
        };
        let idx = self.active_tab;
        let (bucket, region) = {
            let t = &self.tabs[idx];
            (t.bucket.clone(), t.region.clone())
        };
        match s3::presign(&bucket, &o.key, region.as_deref()) {
            Ok(url) => match crate::clipboard::copy(&url) {
                Ok(()) => self.status = format!("copied presigned (5 min) {url}"),
                Err(e) => self.status = format!("copy failed: {e}"),
            },
            Err(e) => self.status = format!("presign failed: {e}"),
        }
    }

    /// `o` — open the AWS console URL for the active bucket/prefix.
    pub fn open_console(&mut self) {
        let tab = self.active();
        let url = s3::console_url(&tab.bucket, &tab.prefix, tab.region.as_deref());
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `d` — arm a delete confirmation for the focused row (objects
    /// only). The UI surfaces the prompt; the next `y` confirms,
    /// any other key cancels.
    pub fn arm_delete(&mut self) {
        let Some(entry) = self.focused_entry().cloned() else {
            return;
        };
        let Entry::Object(o) = entry else {
            self.status = "delete only applies to files".into();
            return;
        };
        let idx = self.active_tab;
        let (bucket, region) = {
            let t = &self.tabs[idx];
            (t.bucket.clone(), t.region.clone())
        };
        self.status = format!(
            "delete {}? press `y` to confirm, any other key to cancel",
            o.key
        );
        self.pending_confirm = Some(PendingConfirm::Delete {
            bucket,
            key: o.key,
            region,
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
                bucket,
                key,
                region,
            } => match s3::delete(&bucket, &key, region.as_deref()) {
                Ok(()) => {
                    self.status = format!("deleted {key}");
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
        match entry {
            Entry::Object(o) => Some(format!("s3://{}/{}", tab.bucket, o.key)),
            Entry::Prefix(p) => Some(format!("s3://{}/{}{}", tab.bucket, tab.prefix, p.name)),
        }
    }
}

fn tab_from_config(b: &Bucket) -> TabState {
    let mut prefix = b.prefix.clone().unwrap_or_default();
    // Normalize: prefix should end in `/` when non-empty so the
    // delimiter-based list works correctly.
    if !prefix.is_empty() && !prefix.ends_with('/') {
        prefix.push('/');
    }
    TabState {
        name: b.name.clone(),
        bucket: b.bucket.clone(),
        region: b.region.clone(),
        prefix,
        prefix_stack: Vec::new(),
        items: Vec::new(),
        selected: 0,
        last_error: None,
        loading: false,
        pending: None,
    }
}

fn cache_path_for(bucket: &str, key: &str) -> PathBuf {
    let mut p = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("mnml-fs-s3");
    p.push(bucket);
    // The key may contain `/`s — that's fine, PathBuf joins them.
    p.push(key);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Bucket;

    #[test]
    fn prefix_normalization_adds_trailing_slash() {
        let t = tab_from_config(&Bucket {
            name: "x".into(),
            bucket: "b".into(),
            prefix: Some("logs/2026".into()),
            region: None,
        });
        assert_eq!(t.prefix, "logs/2026/");
    }

    #[test]
    fn prefix_normalization_preserves_trailing_slash() {
        let t = tab_from_config(&Bucket {
            name: "x".into(),
            bucket: "b".into(),
            prefix: Some("logs/2026/".into()),
            region: None,
        });
        assert_eq!(t.prefix, "logs/2026/");
    }

    #[test]
    fn prefix_normalization_keeps_empty() {
        let t = tab_from_config(&Bucket {
            name: "x".into(),
            bucket: "b".into(),
            prefix: None,
            region: None,
        });
        assert_eq!(t.prefix, "");
    }

    #[test]
    fn cache_path_has_bucket_and_key() {
        let p = cache_path_for("my-bucket", "a/b/c.txt");
        assert!(p.to_string_lossy().contains("mnml-fs-s3"));
        assert!(p.to_string_lossy().contains("my-bucket"));
        assert!(p.to_string_lossy().ends_with("a/b/c.txt"));
    }

    fn empty_app() -> App {
        // Build a minimal App without letting the constructor kick off
        // a live S3 refresh (which would fork a thread and try `aws`).
        App {
            tabs: vec![TabState {
                name: "t".into(),
                bucket: "b".into(),
                region: None,
                prefix: "p/".into(),
                prefix_stack: Vec::new(),
                items: Vec::new(),
                selected: 0,
                last_error: None,
                loading: false,
                pending: None,
            }],
            active_tab: 0,
            status: String::new(),
            pending_confirm: None,
            upload_overlay: None,
            upload_next_id: 1,
        }
    }

    #[test]
    fn upload_fire_enqueues_tasks_with_keys_scoped_to_prefix() {
        let mut app = empty_app();
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.log");
        std::fs::write(&a, b"hello").unwrap();
        std::fs::write(&b, b"world!").unwrap();
        app.upload_fire(vec![a, b]);
        let Some(UploadOverlay::Progress(pg)) = &app.upload_overlay else {
            panic!("expected Progress overlay");
        };
        assert_eq!(pg.tasks.len(), 2);
        assert_eq!(pg.tasks[0].key, "p/a.txt");
        assert_eq!(pg.tasks[1].key, "p/b.log");
        assert_eq!(pg.tasks[0].total, 5);
        assert_eq!(pg.tasks[1].total, 6);
        // Initial state — before any drain/promote — must be Queued.
        assert!(matches!(pg.tasks[0].state, UploadState::Queued));
    }

    #[test]
    fn upload_cancel_hides_overlay() {
        let mut app = empty_app();
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("x");
        std::fs::write(&f, b"x").unwrap();
        app.upload_fire(vec![f]);
        assert!(app.upload_overlay.is_some());
        app.upload_cancel();
        assert!(app.upload_overlay.is_none());
    }

    #[test]
    fn upload_fire_empty_leaves_no_overlay() {
        let mut app = empty_app();
        app.upload_fire(vec![]);
        assert!(app.upload_overlay.is_none());
    }
}
