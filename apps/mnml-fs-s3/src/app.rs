//! App state — bucket tabs, breadcrumb stack, selection. The S3
//! calls happen on a worker thread; the App polls a channel each
//! tick to drain results.

use crate::config::{Bucket, Config};
use crate::s3::{self, Entry};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

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
    /// Active upload prompt — set when the user presses `u`. The
    /// UI surfaces a single-line text input; on submit, we upload
    /// the typed local path to the current prefix.
    pub upload_prompt: Option<UploadPrompt>,
}

#[derive(Debug, Clone)]
pub struct UploadPrompt {
    /// In-progress path the user is typing.
    pub buffer: String,
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
            upload_prompt: None,
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
                        // Keep selection in range across re-list.
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

    /// `u` — start the single-line upload prompt. UI captures all
    /// keys until the user hits Enter (submit) or Esc (cancel).
    pub fn start_upload_prompt(&mut self) {
        let (bucket, prefix) = {
            let tab = self.active();
            (tab.bucket.clone(), tab.prefix.clone())
        };
        self.upload_prompt = Some(UploadPrompt {
            buffer: String::new(),
        });
        self.status = format!(
            "upload to s3://{bucket}/{prefix} — type local path, Enter to send, Esc to cancel"
        );
    }

    /// Append a character to the in-progress upload prompt.
    pub fn upload_append(&mut self, c: char) {
        if let Some(p) = self.upload_prompt.as_mut() {
            p.buffer.push(c);
        }
    }

    /// Backspace — drop the trailing character from the upload prompt.
    pub fn upload_backspace(&mut self) {
        if let Some(p) = self.upload_prompt.as_mut() {
            p.buffer.pop();
        }
    }

    /// Submit the upload — read the path, do `aws s3 cp <local>
    /// s3://bucket/prefix/<basename>`, refresh the listing.
    pub fn upload_submit(&mut self) {
        let Some(prompt) = self.upload_prompt.take() else {
            return;
        };
        let local_str = prompt.buffer.trim().to_string();
        if local_str.is_empty() {
            self.status = "upload cancelled (empty path)".to_string();
            return;
        }
        let local_path = PathBuf::from(shellexpand_tilde(&local_str));
        if !local_path.is_file() {
            self.status = format!("upload failed: {} is not a file", local_path.display());
            return;
        }
        let filename = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload")
            .to_string();
        let idx = self.active_tab;
        let (bucket, prefix, region) = {
            let t = &self.tabs[idx];
            (t.bucket.clone(), t.prefix.clone(), t.region.clone())
        };
        let key = format!("{prefix}{filename}");
        self.status = format!("uploading {local_str} → s3://{bucket}/{key}…");
        match s3::upload(&local_path, &bucket, &key, region.as_deref()) {
            Ok(()) => {
                self.status = format!("uploaded s3://{bucket}/{key}");
                self.refresh_active();
            }
            Err(e) => self.status = format!("upload failed: {e}"),
        }
    }

    /// Esc — cancel the upload prompt.
    pub fn upload_cancel(&mut self) {
        if self.upload_prompt.take().is_some() {
            self.status = "upload cancelled".into();
        }
    }

    /// Backspace / `h` — go up one prefix level.
    pub fn pop_prefix(&mut self) {
        let tab = self.active_mut();
        let Some(prev) = tab.prefix_stack.pop() else {
            // Already at the bucket root.
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

/// Expand a leading `~` to the user's home dir; otherwise pass
/// through unchanged. Same behaviour as `shellexpand::tilde` but
/// without the dep.
fn shellexpand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().to_string();
    }
    if s == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home.to_string_lossy().to_string();
    }
    s.to_string()
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
}
