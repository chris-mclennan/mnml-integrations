//! App state — per-tab item lists, focused-channel history cache,
//! user-name cache, post / search / react input modes.

use crate::config::{Config, Tab};
use crate::slack::{self, Auth, Channel, Message, QUICK_REACTIONS, SearchMatch};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// Job sent from the TUI event loop to the background loader thread.
/// Every blocking HTTPS call lives in [`spawn_loader`]; the event
/// loop just sends jobs and drains [`LoadResult`]s.
#[derive(Debug, Clone)]
enum LoadJob {
    /// Pull `conversations.history` for `channel_id` + best-effort
    /// `users.info` for up to 10 unknown participants. Single
    /// request from the event loop (one arrow press) → single job.
    Detail { channel_id: String, auth: Auth },
}

/// Result the loader thread sends back. The event loop drains these
/// each tick and applies them to App state.
#[derive(Debug)]
enum LoadResult {
    Detail {
        channel_id: String,
        messages: Vec<Message>,
        user_names: HashMap<String, String>,
    },
    DetailFailed {
        channel_id: String,
        error: String,
    },
}

/// Spawn the loader thread. The thread owns no App state — it works
/// purely on `LoadJob`s arriving through `job_rx`, blocks on HTTPS
/// inside `slack::*`, and sends `LoadResult`s back through `res_tx`.
/// Drops itself when `job_rx` closes (App drop drops the Sender).
fn spawn_loader(job_rx: Receiver<LoadJob>, res_tx: Sender<LoadResult>) {
    thread::Builder::new()
        .name("mnml-msg-slack-loader".into())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                match job {
                    LoadJob::Detail { channel_id, auth } => {
                        match slack::conversations_history(&auth, &channel_id, 30) {
                            Ok(msgs) => {
                                let unknown: Vec<String> = msgs
                                    .iter()
                                    .filter_map(|m| m.user.clone())
                                    .collect::<std::collections::HashSet<_>>()
                                    .into_iter()
                                    .take(10)
                                    .collect();
                                let mut user_names: HashMap<String, String> = HashMap::new();
                                for uid in unknown {
                                    if let Ok(u) = slack::users_info(&auth, &uid) {
                                        user_names.insert(uid, u.best_name());
                                    }
                                }
                                let _ = res_tx.send(LoadResult::Detail {
                                    channel_id,
                                    messages: msgs,
                                    user_names,
                                });
                            }
                            Err(e) => {
                                let _ = res_tx.send(LoadResult::DetailFailed {
                                    channel_id,
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

/// 5-min channel-list cache.
const CHANNEL_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    #[allow(dead_code)]
    pub query: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "channels" | "dms" | "search" | "threads" | "canvases" => Ok(Self {
                kind: t.kind.clone(),
                query: t.query.clone(),
            }),
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Channel(Channel),
    SearchHit(SearchMatch),
    /// `threads` tab is a stub in v0.1.
    ThreadPlaceholder,
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
    /// `search` tab: the most-recently-submitted query.
    pub search_query: String,
}

impl TabState {
    fn empty(name: String, spec: TabSpec) -> Self {
        Self {
            name,
            spec,
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
            search_query: String::new(),
        }
    }
}

/// Right-pane state for a focused channel / DM.
pub struct ChannelDetail {
    pub channel_id: String,
    pub messages: Vec<Message>,
    pub last_loaded: Instant,
}

/// Interactive bottom-bar mode. None = passive; otherwise a one-line
/// text input that captures keystrokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Search,
    Post,
    ThreadReply,
}

#[derive(Debug, Clone)]
pub struct InputBar {
    pub mode: InputMode,
    pub buffer: String,
    /// For `ThreadReply` — the `(channel_id, parent_ts)` we're replying to.
    pub thread_target: Option<(String, String)>,
}

/// Reaction picker overlay state — selected index into [`QUICK_REACTIONS`].
#[derive(Debug, Clone)]
pub struct ReactionPicker {
    pub selected: usize,
    pub channel_id: String,
    pub message_ts: String,
}

pub struct App {
    pub cfg: Config,
    pub auth: Auth,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    pub detail: Option<ChannelDetail>,
    /// User-id → resolved display name. Lazy-filled from `users.info`.
    pub user_names: HashMap<String, String>,
    /// Channel-list cache (`types` → (fetched-at, channels)).
    pub channel_cache: HashMap<String, (Instant, Vec<Channel>)>,
    pub input: Option<InputBar>,
    pub reaction_picker: Option<ReactionPicker>,
    /// Last `auth.test` payload — used for the title bar.
    pub team_name: String,
    pub self_user_id: String,
    /// Send half of the loader-thread job channel. Drop = thread exits.
    loader_tx: Sender<LoadJob>,
    /// Receive half of the loader-thread result channel. Drained each
    /// tick via [`Self::drain_load_results`].
    loader_rx: Receiver<LoadResult>,
    /// Channel id currently being loaded (or most-recently requested).
    /// Used to coalesce arrow-key spam — repeated requests for the
    /// SAME channel don't re-queue work. A result for a stale id is
    /// applied if the user has since moved on (the renderer will
    /// just overwrite it shortly), so we don't try to filter on the
    /// receive side.
    pending_channel_id: Option<String>,
    /// `true` between sending a Detail job and receiving its result —
    /// the UI uses this to show a loading indicator.
    pub detail_loading: bool,
}

impl App {
    pub fn new(cfg: Config, auth: Auth) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t)?;
            tabs.push(TabState::empty(t.name.clone(), spec));
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
            detail: None,
            user_names: HashMap::new(),
            channel_cache: HashMap::new(),
            input: None,
            reaction_picker: None,
            team_name: String::new(),
            self_user_id: String::new(),
            loader_tx: job_tx,
            loader_rx: res_rx,
            pending_channel_id: None,
            detail_loading: false,
        };
        // Best-effort auth.test on startup — surfaces a bad token
        // immediately and primes the title bar. Don't hard-fail.
        match slack::auth_test(&app.auth) {
            Ok(t) => {
                app.team_name = t.team;
                app.self_user_id = t.user_id;
            }
            Err(e) => {
                app.status = format!("error: {e}");
            }
        }
        app.refresh_active(false);
        Ok(app)
    }

    /// Drain results from the loader thread and apply them to App
    /// state. Call from the event loop each tick BEFORE rendering.
    /// Returns true when something changed (so the caller can skip
    /// the redraw when nothing did, though in practice we just
    /// redraw unconditionally each tick).
    pub fn drain_load_results(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.loader_rx.try_recv() {
                Ok(LoadResult::Detail {
                    channel_id,
                    messages,
                    user_names,
                }) => {
                    for (uid, name) in user_names {
                        self.user_names.entry(uid).or_insert(name);
                    }
                    self.detail = Some(ChannelDetail {
                        channel_id: channel_id.clone(),
                        messages,
                        last_loaded: Instant::now(),
                    });
                    // Clear loading flag IF this matches the most
                    // recent request — a stale result still applies
                    // its messages (no harm) but doesn't toggle the
                    // indicator off if the user has moved on.
                    if self.pending_channel_id.as_ref() == Some(&channel_id) {
                        self.detail_loading = false;
                    }
                    changed = true;
                }
                Ok(LoadResult::DetailFailed { channel_id, error }) => {
                    if self.pending_channel_id.as_ref() == Some(&channel_id) {
                        self.detail_loading = false;
                    }
                    self.status = format!("error: {error}");
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
            self.detail = None;
            if self.tabs[idx].items.is_empty() && self.tabs[idx].last_error.is_none() {
                self.refresh_active(false);
            } else {
                self.maybe_load_detail();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.items.is_empty() {
            return;
        }
        let n = tab.items.len() as isize;
        let cur = tab.selected as isize;
        let next = (cur + delta).clamp(0, n - 1);
        tab.selected = next as usize;
        self.maybe_load_detail();
    }

    /// On channel/dm tabs only: lazy-load `conversations.history` for
    /// the focused channel into the detail pane.
    ///
    /// Non-blocking — sends a job to the loader thread and returns
    /// immediately. Results arrive via [`Self::drain_load_results`]
    /// on the next tick. The prior implementation blocked the TUI
    /// event loop on up to 11 sequential 30s HTTPS calls per arrow
    /// keypress (sibling-crates-hunt-2026-06-08 HIGH-severity
    /// finding); arrowing through 10 channels could freeze the
    /// crossterm input layer for over 5 minutes.
    fn maybe_load_detail(&mut self) {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        if kind != "channels" && kind != "dms" {
            self.detail = None;
            self.detail_loading = false;
            self.pending_channel_id = None;
            return;
        }
        let Some(Item::Channel(c)) = self.tabs[idx].items.get(self.tabs[idx].selected).cloned()
        else {
            self.detail = None;
            self.detail_loading = false;
            self.pending_channel_id = None;
            return;
        };
        // Skip refetch if this is already the focused detail and
        // we've loaded within the last few seconds — saves a
        // round-trip on simple back-and-forth selection movement.
        if let Some(d) = &self.detail
            && d.channel_id == c.id
            && d.last_loaded.elapsed() < Duration::from_secs(15)
        {
            return;
        }
        // Coalesce: already requested THIS channel's detail and a
        // result hasn't arrived yet ⇒ don't re-queue.
        if self.detail_loading && self.pending_channel_id.as_ref() == Some(&c.id) {
            return;
        }
        self.pending_channel_id = Some(c.id.clone());
        self.detail_loading = true;
        if let Err(e) = self.loader_tx.send(LoadJob::Detail {
            channel_id: c.id.clone(),
            auth: self.auth.clone(),
        }) {
            // Loader thread is gone — fall back to clearing state
            // and showing an error. Should never happen in
            // practice (thread lives for App's lifetime).
            self.detail_loading = false;
            self.pending_channel_id = None;
            self.status = format!("loader send failed: {e}");
        }
    }

    /// `r` — force a fresh channel-list pull (bypass cache).
    pub fn refresh_force(&mut self) {
        self.refresh_active(true);
    }

    pub fn refresh_active(&mut self, force: bool) {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        let name = self.tabs[idx].name.clone();
        self.tabs[idx].loading = true;

        let res: Result<Vec<Item>> = match kind.as_str() {
            "channels" => {
                let types = "public_channel,private_channel";
                let filter = self.cfg.channels.clone();
                self.list_channels_cached(types, force).map(|chans| {
                    sort_channels(chans, &filter)
                        .into_iter()
                        .filter(|c| filter.allows(&c.name))
                        .map(Item::Channel)
                        .collect()
                })
            }
            "dms" => {
                let types = "im,mpim";
                self.list_channels_cached(types, force)
                    .map(|chans| chans.into_iter().map(Item::Channel).collect())
            }
            "search" => {
                // Search-tab refresh re-runs the last-submitted query.
                let q = self.tabs[idx].search_query.clone();
                if q.trim().is_empty() {
                    self.status = "(search): press / to enter a query".into();
                    self.tabs[idx].loading = false;
                    return;
                }
                slack::search_messages(&self.auth, &q)
                    .map(|hits| hits.into_iter().map(Item::SearchHit).collect())
            }
            "threads" => {
                self.tabs[idx].loading = false;
                self.tabs[idx].items = vec![Item::ThreadPlaceholder];
                self.tabs[idx].selected = 0;
                self.tabs[idx].last_loaded = Some(Instant::now());
                self.status = "threads: (v0.2 — needs scan across recent channels)".into();
                return;
            }
            "canvases" => {
                // 2026-07-22 — v0.1 stub. Real work: `files.list?
                // type=canvas` returns canvas file metadata; each
                // canvas can then be pulled via `files.info` and
                // its blocks rendered. Non-trivial (canvas has its
                // own block model — rich text, embeds, actions).
                self.tabs[idx].loading = false;
                self.tabs[idx].items = vec![Item::ThreadPlaceholder];
                self.tabs[idx].selected = 0;
                self.tabs[idx].last_loaded = Some(Instant::now());
                self.status =
                    "canvases: (v0.2 — files.list?type=canvas + block renderer needed)".into();
                return;
            }
            _ => unreachable!("validated in TabSpec::resolve"),
        };

        let t = &mut self.tabs[idx];
        t.loading = false;
        match res {
            Ok(items) => {
                let n = items.len();
                t.items = items;
                t.selected = t.selected.min(n.saturating_sub(1));
                t.last_loaded = Some(Instant::now());
                t.last_error = None;
                self.status = format!("{name}: {n} item{}", if n == 1 { "" } else { "s" });
                self.maybe_load_detail();
            }
            Err(e) => {
                t.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    fn list_channels_cached(&mut self, types: &str, force: bool) -> Result<Vec<Channel>> {
        if !force
            && let Some((fetched, chans)) = self.channel_cache.get(types)
            && fetched.elapsed() < CHANNEL_CACHE_TTL
        {
            return Ok(chans.clone());
        }
        let chans = slack::conversations_list(&self.auth, types)?;
        self.channel_cache
            .insert(types.to_string(), (Instant::now(), chans.clone()));
        Ok(chans)
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.items.get(t.selected)
    }

    /// `Enter` — open a thread view. v0.1: bring the focused channel's
    /// detail pane to front + flash a hint. Real threaded view is v0.2.
    pub fn open_thread(&mut self) {
        match self.focused_item() {
            Some(Item::Channel(_)) => {
                self.maybe_load_detail();
                self.status = "loaded history (thread-view v0.2)".into();
            }
            Some(Item::SearchHit(hit)) => {
                self.status = format!("search hit ts={} (thread-view v0.2)", hit.ts);
            }
            Some(Item::ThreadPlaceholder) | None => {
                self.status = "nothing to open".into();
            }
        }
    }

    /// `/` — open the search input bar (only meaningful on the search tab).
    pub fn begin_search(&mut self) {
        if self.active().spec.kind != "search" {
            // Allow it from any tab — switch to the first search tab.
            if let Some(i) = self.tabs.iter().position(|t| t.spec.kind == "search") {
                self.switch_tab(i);
            } else {
                self.status = "no search tab configured".into();
                return;
            }
        }
        let initial = self.active().search_query.clone();
        self.input = Some(InputBar {
            mode: InputMode::Search,
            buffer: initial,
            thread_target: None,
        });
    }

    /// `p` — open the post input bar. Requires a focused channel.
    pub fn begin_post(&mut self) {
        let Some(channel) = self.focused_channel() else {
            self.status = "no channel under cursor".into();
            return;
        };
        let _ = channel;
        self.input = Some(InputBar {
            mode: InputMode::Post,
            buffer: String::new(),
            thread_target: None,
        });
    }

    /// `T` — open the thread-reply input bar. Requires a focused
    /// channel and a focused message in the detail pane.
    pub fn begin_thread_reply(&mut self) {
        let Some(channel) = self.focused_channel() else {
            self.status = "no channel under cursor".into();
            return;
        };
        let channel_id = channel.id.clone();
        let Some(detail) = &self.detail else {
            self.status = "no detail panel loaded".into();
            return;
        };
        // Pick the most-recent message in the detail pane as the
        // parent. v0.2 will add a cursor inside the detail pane.
        let Some(msg) = detail.messages.last() else {
            self.status = "no messages in channel".into();
            return;
        };
        let parent_ts = msg.thread_ts.clone().unwrap_or_else(|| msg.ts.clone());
        self.input = Some(InputBar {
            mode: InputMode::ThreadReply,
            buffer: String::new(),
            thread_target: Some((channel_id, parent_ts)),
        });
    }

    /// `R` — open the reaction picker overlay.
    pub fn begin_reaction(&mut self) {
        let Some(channel) = self.focused_channel() else {
            self.status = "no channel under cursor".into();
            return;
        };
        let channel_id = channel.id.clone();
        let Some(detail) = &self.detail else {
            self.status = "no detail panel loaded".into();
            return;
        };
        let Some(msg) = detail.messages.last() else {
            self.status = "no messages in channel".into();
            return;
        };
        self.reaction_picker = Some(ReactionPicker {
            selected: 0,
            channel_id,
            message_ts: msg.ts.clone(),
        });
    }

    pub fn cancel_input(&mut self) {
        self.input = None;
        self.status = "cancelled".into();
    }

    pub fn cancel_reaction(&mut self) {
        self.reaction_picker = None;
        self.status = "cancelled".into();
    }

    /// Commit the current input bar (`Enter`).
    pub fn submit_input(&mut self) {
        let Some(bar) = self.input.take() else {
            return;
        };
        match bar.mode {
            InputMode::Search => {
                let q = bar.buffer.trim().to_string();
                if q.is_empty() {
                    self.status = "search cancelled (empty query)".into();
                    return;
                }
                let idx = self.active_tab;
                self.tabs[idx].search_query = q;
                self.refresh_active(false);
            }
            InputMode::Post => {
                let Some(channel) = self.focused_channel() else {
                    self.status = "lost focused channel".into();
                    return;
                };
                let channel_id = channel.id.clone();
                let channel_name = channel.display_name();
                let text = bar.buffer.trim().to_string();
                if text.is_empty() {
                    self.status = "empty post".into();
                    return;
                }
                match slack::chat_post_message(&self.auth, &channel_id, &text, None) {
                    Ok(_) => {
                        self.status = format!("posted to {channel_name}");
                        // Force a detail refresh so the new message shows.
                        self.detail = None;
                        self.maybe_load_detail();
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
            InputMode::ThreadReply => {
                let Some((channel_id, parent_ts)) = bar.thread_target.clone() else {
                    self.status = "lost thread target".into();
                    return;
                };
                let text = bar.buffer.trim().to_string();
                if text.is_empty() {
                    self.status = "empty thread reply".into();
                    return;
                }
                match slack::chat_post_message(&self.auth, &channel_id, &text, Some(&parent_ts)) {
                    Ok(_) => {
                        self.status = "thread reply sent".into();
                        self.detail = None;
                        self.maybe_load_detail();
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
        }
    }

    /// `Enter` on the reaction picker.
    pub fn submit_reaction(&mut self) {
        let Some(picker) = self.reaction_picker.take() else {
            return;
        };
        let Some(emoji) = QUICK_REACTIONS.get(picker.selected) else {
            self.status = "reaction picker out of range".into();
            return;
        };
        match slack::reactions_add(&self.auth, &picker.channel_id, &picker.message_ts, emoji) {
            Ok(()) => self.status = format!("reacted :{emoji}:"),
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    /// `y` — copy the permalink for the focused message (channel/DM)
    /// or the focused search hit.
    pub fn yank_permalink(&mut self) {
        match self.focused_item() {
            Some(Item::SearchHit(hit)) => {
                let url = hit.permalink.clone().unwrap_or_default();
                if url.is_empty() {
                    self.status = "no permalink on search hit".into();
                    return;
                }
                let n = url.chars().count();
                match crate::clipboard::copy(&url) {
                    Ok(()) => self.status = format!("copied permalink ({n} chars)"),
                    Err(e) => self.status = format!("copy failed: {e}"),
                }
            }
            Some(Item::Channel(_)) => {
                let Some(channel) = self.focused_channel() else {
                    self.status = "lost focused channel".into();
                    return;
                };
                let channel_id = channel.id.clone();
                let Some(detail) = &self.detail else {
                    self.status = "no detail panel loaded".into();
                    return;
                };
                let Some(msg) = detail.messages.last() else {
                    self.status = "no messages in channel".into();
                    return;
                };
                let ts = msg.ts.clone();
                match slack::chat_get_permalink(&self.auth, &channel_id, &ts) {
                    Ok(url) => {
                        let n = url.chars().count();
                        match crate::clipboard::copy(&url) {
                            Ok(()) => self.status = format!("copied permalink ({n} chars)"),
                            Err(e) => self.status = format!("copy failed: {e}"),
                        }
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
            Some(Item::ThreadPlaceholder) | None => {
                self.status = "nothing to copy".into();
            }
        }
    }

    /// Tick — periodic background refresh on the current tab.
    pub fn tick(&mut self) -> bool {
        // Drain any results the loader thread has produced. Always
        // run this — channel/dm tabs use it for `maybe_load_detail`
        // results, so we can't gate on `kind`.
        let loader_changed = self.drain_load_results();
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        // Search + threads don't auto-refresh.
        if kind == "search" || kind == "threads" {
            return loader_changed;
        }
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return false;
        }
        let stale = match self.tabs[idx].last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].loading && self.input.is_none() {
            self.refresh_active(false);
            true
        } else {
            loader_changed
        }
    }

    pub fn focused_channel(&self) -> Option<&Channel> {
        match self.focused_item()? {
            Item::Channel(c) => Some(c),
            _ => None,
        }
    }

    /// `p` — pin (or unpin) the focused channel. Pinned channels
    /// sort to the top of the list unconditionally. Persists to
    /// `[channels].pin` in the config file via `toml_edit` so
    /// comments survive.
    pub fn toggle_pin_focused_channel(&mut self) {
        if self.active().spec.kind != "channels" {
            self.status = "pin only works on the channels tab".into();
            return;
        }
        let Some(name) = self.focused_channel().map(|c| c.name.clone()) else {
            self.status = "no channel under cursor".into();
            return;
        };
        if name.is_empty() {
            self.status = "channel has no name — skipping".into();
            return;
        }
        let already = self.cfg.channels.is_pinned(&name);
        let toast = if already {
            // Unpin — remove from both the in-memory list and the file.
            let bare = name.trim_start_matches('#').to_lowercase();
            self.cfg
                .channels
                .pin
                .retain(|p| p.trim_start_matches('#').to_lowercase() != bare);
            match crate::config::remove_pin_channel(&name) {
                Ok(_) => format!("unpinned #{name}"),
                Err(e) => format!("unpinned #{name} (config write failed: {e})"),
            }
        } else {
            let with_hash = format!("#{name}");
            self.cfg.channels.pin.push(with_hash);
            match crate::config::append_pin_channel(&name) {
                Ok(_) => format!("pinned #{name} (sorts to top)"),
                Err(e) => format!("pinned #{name} (config write failed: {e})"),
            }
        };
        self.refresh_active(false);
        self.status = toast;
    }

    /// `x` — hide the focused channel from the list and persist
    /// it to the config's `[channels].hide` array so the choice
    /// survives restart. To unhide, edit the config file.
    ///
    /// Refuses on non-`channels` tabs — the filter only applies to
    /// the channels tab, so hiding a DM (which shares the
    /// `focused_channel` accessor) would silently add garbage to
    /// the config with no user-visible effect (2026-07-22 tester
    /// finding).
    pub fn hide_focused_channel(&mut self) {
        if self.active().spec.kind != "channels" {
            self.status = "hide only works on the channels tab".into();
            return;
        }
        let Some(name) = self.focused_channel().map(|c| c.name.clone()) else {
            self.status = "no channel under cursor".into();
            return;
        };
        if name.is_empty() {
            self.status = "channel has no name — skipping".into();
            return;
        }
        // Add to in-memory config so the very next refresh drops
        // the row. Also add the bumper-`#` form so users editing
        // the file by hand see a friendly value.
        let with_hash = format!("#{name}");
        if !self
            .cfg
            .channels
            .hide
            .iter()
            .any(|h| h.trim_start_matches('#').eq_ignore_ascii_case(&name))
        {
            self.cfg.channels.hide.push(with_hash);
        }
        // Persist via a comment-preserving `toml_edit` append.
        // Failure is toasted but the in-memory hide still takes
        // effect for the session.
        let toast = match crate::config::append_hide_channel(&name) {
            Ok(_) => format!("hidden #{name} (persisted to config)"),
            Err(e) => format!("hidden #{name} (config write failed: {e})"),
        };
        // Re-filter without a fresh HTTPS call — the cache holds
        // the raw list; refresh_active runs the filter over it.
        // 2026-07-22 — set status AFTER refresh_active so the
        // "hidden" toast survives (refresh_active clobbers status
        // with "channels: N items" otherwise).
        self.refresh_active(false);
        self.status = toast;
    }

    pub fn resolve_user(&self, uid: &str) -> String {
        self.user_names
            .get(uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }
}

/// Sort channels for the `channels` tab. Pinned rows (per
/// `[channels].pin`) sort to the very top in their config-file
/// order. When `[channels].show` is non-empty, remaining rows
/// sort by their position in `show` — a channel-visibility
/// whitelist that also acts as a display-order (user asked for
/// "define which ones I want and have it remembered" +
/// user-declared order wins over alphabetical). Everything not
/// pinned + not in `show` falls back to members-first +
/// alphabetical (or if `show` is empty, everyone gets that
/// default). 2026-08-01.
fn sort_channels(mut chans: Vec<Channel>, filter: &crate::config::ChannelFilter) -> Vec<Channel> {
    chans.sort_by(|a, b| {
        let ap = filter.pin_position(&a.name);
        let bp = filter.pin_position(&b.name);
        match (ap, bp) {
            (Some(a_idx), Some(b_idx)) => a_idx.cmp(&b_idx),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => {
                let as_ = filter.show_position(&a.name);
                let bs = filter.show_position(&b.name);
                match (as_, bs) {
                    (Some(ai), Some(bi)) => ai.cmp(&bi),
                    // A channel in `show` beats one that isn't
                    // (defensive — `allows()` should have already
                    // filtered non-show channels out when show is
                    // non-empty, but sorting defensively keeps the
                    // relative ordering stable if the filter is
                    // bypassed).
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => b
                        .is_member
                        .cmp(&a.is_member)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                }
            }
        }
    });
    chans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolves_known_kinds() {
        for kind in &["channels", "dms", "search", "threads"] {
            let t = Tab {
                name: "x".into(),
                kind: kind.to_string(),
                query: None,
            };
            assert!(TabSpec::resolve(&t).is_ok(), "{kind}");
        }
    }

    #[test]
    fn tab_spec_rejects_unknown() {
        let t = Tab {
            name: "x".into(),
            kind: "monkeys".into(),
            query: None,
        };
        assert!(TabSpec::resolve(&t).is_err());
    }

    #[test]
    fn sort_channels_puts_members_first() {
        let chans = vec![
            mk_channel("zebra", false),
            mk_channel("alpha", false),
            mk_channel("delta", true),
            mk_channel("bravo", true),
        ];
        let sorted = sort_channels(chans, &crate::config::ChannelFilter::default());
        assert!(sorted[0].is_member);
        assert!(sorted[1].is_member);
        assert!(!sorted[2].is_member);
        assert!(!sorted[3].is_member);
        assert_eq!(sorted[0].name, "bravo");
        assert_eq!(sorted[1].name, "delta");
    }

    #[test]
    fn sort_channels_pinned_sort_to_top_in_config_order() {
        // Pinned rows should appear before ANY non-pinned row —
        // even members. Config order wins within the pinned group.
        let chans = vec![
            mk_channel("zebra", true),
            mk_channel("alpha", false),
            mk_channel("delta", true),
            mk_channel("bravo", false),
        ];
        let filter = crate::config::ChannelFilter {
            pin: vec!["#bravo".into(), "#delta".into()],
            ..Default::default()
        };
        let sorted = sort_channels(chans, &filter);
        assert_eq!(sorted[0].name, "bravo", "first pin wins");
        assert_eq!(sorted[1].name, "delta", "second pin next");
        // Remaining follow the members-first tie-break.
        assert!(sorted[2].is_member);
        assert!(!sorted[3].is_member);
    }

    #[test]
    fn sort_channels_show_order_wins_over_alphabetical() {
        // 2026-08-01 — the `show` list acts as a whitelist AND
        // a user-declared display order. Non-pinned channels
        // sort by their `show` position (case-insensitive on
        // the bare name), not alphabetically. Channels not in
        // `show` still fall to members-first + alphabetical
        // (defense-in-depth; `allows()` normally filters them
        // out first).
        let chans = vec![
            mk_channel("alpha", true),
            mk_channel("bravo", true),
            mk_channel("deploys", true),
            mk_channel("eng-general", true),
            mk_channel("team-tattle", true),
        ];
        let filter = crate::config::ChannelFilter {
            show: vec![
                "eng-general".into(),
                "#team-tattle".into(),
                "DEPLOYS".into(),
            ],
            ..Default::default()
        };
        let sorted = sort_channels(chans, &filter);
        // First 3 are exactly the show order.
        assert_eq!(sorted[0].name, "eng-general", "show[0]");
        assert_eq!(sorted[1].name, "team-tattle", "show[1] — # prefix ignored");
        assert_eq!(sorted[2].name, "deploys", "show[2] — case-insensitive");
        // Non-show channels come after, alphabetical.
        assert_eq!(sorted[3].name, "alpha");
        assert_eq!(sorted[4].name, "bravo");
    }

    fn mk_channel(name: &str, member: bool) -> Channel {
        Channel {
            id: name.to_uppercase(),
            name: name.to_string(),
            is_channel: true,
            is_group: false,
            is_im: false,
            is_mpim: false,
            is_private: false,
            is_archived: false,
            is_member: member,
            num_members: Some(1),
            topic: None,
            user: None,
            last_read: None,
            purpose: None,
        }
    }
}
