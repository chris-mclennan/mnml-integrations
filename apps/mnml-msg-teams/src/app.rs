//! App state — per-tab item lists + selection cursor + right-pane
//! message detail.

use crate::config::{Config, Tab};
use crate::teams::{Channel, Chat, GraphClient, Message, Team};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "teams" | "chats" | "search" | "threads" => Ok(Self {
                kind: t.kind.clone(),
            }),
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

/// One row in a tab's list. The list is a flat `Vec<Item>` regardless
/// of tab kind — for the `teams` tab the channel rows are inlined
/// under their team when the team is expanded.
#[derive(Debug, Clone)]
pub enum Item {
    Team {
        team: Team,
        expanded: bool,
        channels_loaded: bool,
    },
    Channel {
        team_id: String,
        channel: Channel,
    },
    Chat(Chat),
    Message(Message),
    /// `search` tab placeholder before the user types a query.
    SearchPrompt,
    /// `threads` tab placeholder for v0.1.
    Placeholder(String),
}

impl Item {
    pub fn primary_label(&self) -> String {
        match self {
            Item::Team { team, expanded, .. } => {
                let arrow = if *expanded { "▾" } else { "▸" };
                let name = team
                    .display_name
                    .clone()
                    .unwrap_or_else(|| "(unnamed)".into());
                format!("{arrow} {name}")
            }
            Item::Channel { channel, .. } => {
                let name = channel
                    .display_name
                    .clone()
                    .unwrap_or_else(|| "(unnamed)".into());
                format!("    ▸ {name}")
            }
            Item::Chat(c) => chat_label(c),
            Item::Message(m) => m.author(),
            Item::SearchPrompt => "(press `/` to search messages)".into(),
            Item::Placeholder(s) => s.clone(),
        }
    }

    pub fn secondary_label(&self) -> String {
        match self {
            Item::Team { team, .. } => team.description.clone().unwrap_or_default(),
            Item::Channel { channel, .. } => channel.description.clone().unwrap_or_default(),
            Item::Chat(c) => c
                .last_updated_date_time
                .as_deref()
                .map(short_ts)
                .unwrap_or_default(),
            Item::Message(m) => {
                let body = m.body_text();
                let first = body.lines().next().unwrap_or("");
                let trimmed: String = first.chars().take(80).collect();
                let ts = m
                    .created_date_time
                    .as_deref()
                    .map(short_ts)
                    .unwrap_or_default();
                if ts.is_empty() {
                    trimmed
                } else {
                    format!("{ts} · {trimmed}")
                }
            }
            Item::SearchPrompt => String::new(),
            Item::Placeholder(_) => String::new(),
        }
    }
}

fn chat_label(c: &Chat) -> String {
    if let Some(t) = &c.topic
        && !t.is_empty()
    {
        return t.clone();
    }
    // 1:1 → other participant's name. With "members" expanded, just
    // join the display names (skips current user since the cache
    // doesn't know us by id here).
    let names: Vec<&str> = c
        .members
        .iter()
        .filter_map(|m| m.display_name.as_deref())
        .take(3)
        .collect();
    if names.is_empty() {
        format!("(chat {})", short_id(&c.id))
    } else {
        names.join(", ")
    }
}

fn short_id(s: &str) -> String {
    s.chars().take(12).collect::<String>() + if s.chars().count() > 12 { "…" } else { "" }
}

pub fn short_ts(ts: &str) -> String {
    // `2026-06-07T12:34:56.789Z` → `06-07 12:34`. Best-effort.
    if let Some((date, rest)) = ts.split_once('T') {
        let date_short: String = date.chars().skip(5).take(5).collect();
        let time: String = rest.chars().take(5).collect();
        return format!("{date_short} {time}");
    }
    ts.to_string()
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
    /// `search` tab: in-progress query buffer when `search_mode` true.
    pub search_query: String,
    pub search_mode: bool,
    /// Right-pane content for the focused list item — last ~30
    /// messages for a channel/chat, an info block for a team.
    pub detail_messages: Vec<Message>,
    pub detail_kind: DetailKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailKind {
    None,
    /// Channel scrollback. `(team_id, channel_id)`.
    Channel(String, String),
    /// Chat scrollback. `chat_id`.
    Chat(String),
    /// Single message (search hit).
    Message,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
            search_query: String::new(),
            search_mode: false,
            detail_messages: Vec::new(),
            detail_kind: DetailKind::None,
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
    pub graph: Arc<GraphClient>,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// Post buffer (when posting via `p` or replying via `T`).
    pub post_mode: Option<PostMode>,
    pub post_buffer: String,
    /// Send half of the loader-thread job channel. Drop = thread exits.
    loader_tx: std::sync::mpsc::Sender<LoadJob>,
    /// Receive half of the loader-thread result channel. Drained
    /// each tick via [`Self::drain_load_results`].
    loader_rx: std::sync::mpsc::Receiver<LoadResult>,
    /// `(tab_idx, DetailKind)` currently in flight from the loader.
    /// Used to coalesce arrow-key spam — repeated requests for the
    /// same target don't re-queue.
    pending: Option<(usize, DetailKind)>,
    /// `true` between sending a Detail job and receiving its result.
    pub detail_loading: bool,
}

/// Job sent from the TUI event loop to the background loader thread.
/// Every blocking HTTPS call lives in [`spawn_loader`]; the event
/// loop just sends jobs and drains [`LoadResult`]s.
enum LoadJob {
    ChannelMessages {
        tab_idx: usize,
        team_id: String,
        channel_id: String,
        graph: Arc<GraphClient>,
    },
    ChatMessages {
        tab_idx: usize,
        chat_id: String,
        graph: Arc<GraphClient>,
    },
}

/// Result the loader thread sends back. The event loop drains these
/// each tick and applies them to App state.
enum LoadResult {
    ChannelMessages {
        tab_idx: usize,
        team_id: String,
        channel_id: String,
        messages: Vec<Message>,
        error: Option<String>,
    },
    ChatMessages {
        tab_idx: usize,
        chat_id: String,
        messages: Vec<Message>,
        error: Option<String>,
    },
}

/// Spawn the loader thread. The thread owns no App state — it works
/// purely on `LoadJob`s arriving through `job_rx`, blocks on HTTPS
/// inside `GraphClient::*`, and sends `LoadResult`s back through
/// `res_tx`. Drops itself when `job_rx` closes (App drop drops the
/// Sender).
fn spawn_loader(
    job_rx: std::sync::mpsc::Receiver<LoadJob>,
    res_tx: std::sync::mpsc::Sender<LoadResult>,
) {
    std::thread::Builder::new()
        .name("mnml-msg-teams-loader".into())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                match job {
                    LoadJob::ChannelMessages {
                        tab_idx,
                        team_id,
                        channel_id,
                        graph,
                    } => {
                        let (messages, error) = match graph.channel_messages(&team_id, &channel_id)
                        {
                            Ok(m) => (m, None),
                            Err(e) => (Vec::new(), Some(e.to_string())),
                        };
                        let _ = res_tx.send(LoadResult::ChannelMessages {
                            tab_idx,
                            team_id,
                            channel_id,
                            messages,
                            error,
                        });
                    }
                    LoadJob::ChatMessages {
                        tab_idx,
                        chat_id,
                        graph,
                    } => {
                        let (messages, error) = match graph.chat_messages(&chat_id) {
                            Ok(m) => (m, None),
                            Err(e) => (Vec::new(), Some(e.to_string())),
                        };
                        let _ = res_tx.send(LoadResult::ChatMessages {
                            tab_idx,
                            chat_id,
                            messages,
                            error,
                        });
                    }
                }
            }
        })
        .expect("spawn loader thread");
}

#[derive(Debug, Clone)]
pub enum PostMode {
    Channel {
        team_id: String,
        channel_id: String,
    },
    Chat {
        chat_id: String,
    },
    /// Threaded reply in a channel.
    ChannelReply {
        team_id: String,
        channel_id: String,
        message_id: String,
    },
}

impl App {
    pub fn new(cfg: Config, graph: GraphClient) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t)?;
            tabs.push(TabState {
                name: t.name.clone(),
                spec,
                data: ItemsTab::empty(),
            });
        }
        let (job_tx, job_rx) = std::sync::mpsc::channel();
        let (res_tx, res_rx) = std::sync::mpsc::channel();
        spawn_loader(job_rx, res_tx);
        let mut app = App {
            cfg,
            graph: Arc::new(graph),
            tabs,
            active_tab: 0,
            status: String::new(),
            post_mode: None,
            post_buffer: String::new(),
            loader_tx: job_tx,
            loader_rx: res_rx,
            pending: None,
            detail_loading: false,
        };
        app.refresh_active();
        Ok(app)
    }

    /// Drain results from the loader thread and apply them to App
    /// state. Call from the event loop each tick BEFORE rendering.
    pub fn drain_load_results(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.loader_rx.try_recv() {
                Ok(LoadResult::ChannelMessages {
                    tab_idx,
                    team_id,
                    channel_id,
                    messages,
                    error,
                }) => {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let kind = DetailKind::Channel(team_id, channel_id);
                        // Apply only if the user hasn't already
                        // moved past this target (otherwise the
                        // stale result would clobber a fresher
                        // detail_kind).
                        if tab.data.detail_kind == kind {
                            tab.data.detail_messages = messages;
                            if let Some(e) = error {
                                self.status = format!("error: {e}");
                            }
                        }
                    }
                    self.clear_pending_if(tab_idx);
                    changed = true;
                }
                Ok(LoadResult::ChatMessages {
                    tab_idx,
                    chat_id,
                    messages,
                    error,
                }) => {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let kind = DetailKind::Chat(chat_id);
                        if tab.data.detail_kind == kind {
                            tab.data.detail_messages = messages;
                            if let Some(e) = error {
                                self.status = format!("error: {e}");
                            }
                        }
                    }
                    self.clear_pending_if(tab_idx);
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.status = "loader thread died".to_string();
                    break;
                }
            }
        }
        changed
    }

    fn clear_pending_if(&mut self, tab_idx: usize) {
        if let Some((pi, _)) = &self.pending
            && *pi == tab_idx
        {
            self.pending = None;
            self.detail_loading = false;
        }
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
        // Update right pane based on what's under the cursor.
        // We can't borrow self mutably twice — defer to a helper.
        self.refresh_detail_for_selection();
    }

    /// Look at the focused item and load detail (channel/chat
    /// scrollback) if appropriate. Cheap when the detail kind already
    /// matches.
    /// Non-blocking — sends a job to the loader thread and returns
    /// immediately. Results arrive via [`Self::drain_load_results`]
    /// on the next tick. The prior implementation blocked the TUI
    /// event loop on a 30s-timeout HTTPS call per arrow keypress
    /// (sibling-crates-hunt-2026-06-08 HIGH-severity finding,
    /// mnml-msg-slack had the same pattern fixed in ddb43a2).
    pub fn refresh_detail_for_selection(&mut self) {
        let idx = self.active_tab;
        let Some(item) = self.tabs[idx]
            .data
            .items
            .get(self.tabs[idx].data.selected)
            .cloned()
        else {
            return;
        };
        match item {
            Item::Channel { team_id, channel } => {
                let want = DetailKind::Channel(team_id.clone(), channel.id.clone());
                if self.tabs[idx].data.detail_kind == want {
                    return;
                }
                // Coalesce: same target already in flight.
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|(pi, k)| *pi == idx && *k == want)
                {
                    return;
                }
                self.tabs[idx].data.detail_kind = want.clone();
                self.pending = Some((idx, want));
                self.detail_loading = true;
                let _ = self.loader_tx.send(LoadJob::ChannelMessages {
                    tab_idx: idx,
                    team_id,
                    channel_id: channel.id,
                    graph: self.graph.clone(),
                });
            }
            Item::Chat(c) => {
                let want = DetailKind::Chat(c.id.clone());
                if self.tabs[idx].data.detail_kind == want {
                    return;
                }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|(pi, k)| *pi == idx && *k == want)
                {
                    return;
                }
                self.tabs[idx].data.detail_kind = want.clone();
                self.pending = Some((idx, want));
                self.detail_loading = true;
                let _ = self.loader_tx.send(LoadJob::ChatMessages {
                    tab_idx: idx,
                    chat_id: c.id,
                    graph: self.graph.clone(),
                });
            }
            Item::Message(m) => {
                self.tabs[idx].data.detail_kind = DetailKind::Message;
                self.tabs[idx].data.detail_messages = vec![m];
            }
            _ => {
                self.tabs[idx].data.detail_kind = DetailKind::None;
                self.tabs[idx].data.detail_messages.clear();
            }
        }
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;

        let g = self.graph.clone();
        let result: Result<Vec<Item>> = match kind.as_str() {
            "teams" => g.joined_teams().map(|ts| {
                ts.into_iter()
                    .map(|t| Item::Team {
                        team: t,
                        expanded: false,
                        channels_loaded: false,
                    })
                    .collect()
            }),
            "chats" => g
                .list_chats()
                .map(|cs| cs.into_iter().map(Item::Chat).collect()),
            "search" => Ok(vec![Item::SearchPrompt]),
            "threads" => Ok(vec![Item::Placeholder(
                "threads tab — v0.1 stub (todo: focused thread view)".into(),
            )]),
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
                let kind_label = match kind.as_str() {
                    "teams" => "teams",
                    "chats" => "chats",
                    "search" => "ready (press / )",
                    "threads" => "(stub)",
                    _ => "items",
                };
                self.status = format!("{name}: {count} {kind_label}");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
        self.refresh_detail_for_selection();
    }

    pub fn tick(&mut self) -> bool {
        // Drain any results the loader thread produced before
        // deciding whether to auto-refresh. Results may toggle
        // `detail_loading` off, which the renderer reads.
        let loader_changed = self.drain_load_results();
        let idx = self.active_tab;
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return loader_changed;
        }
        let stale = match self.tabs[idx].data.last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].data.loading && self.post_mode.is_none() {
            // Don't auto-refresh while the user is mid-post.
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

    /// `Enter` — on a team row, expand/collapse channels. On a
    /// channel/chat, focus has already loaded detail; Enter is a
    /// no-op there. On a search-result message, open the permalink.
    pub fn enter(&mut self) {
        let idx = self.active_tab;
        let sel = self.tabs[idx].data.selected;
        let item_clone = self.tabs[idx].data.items.get(sel).cloned();
        match item_clone {
            Some(Item::Team {
                team,
                expanded,
                channels_loaded,
            }) => {
                if expanded {
                    // Collapse — drop following channel rows for this team.
                    let team_id = team.id.clone();
                    let after = sel + 1;
                    let mut end = after;
                    while let Some(Item::Channel { team_id: tid, .. }) =
                        self.tabs[idx].data.items.get(end)
                    {
                        if *tid != team_id {
                            break;
                        }
                        end += 1;
                    }
                    self.tabs[idx].data.items.drain(after..end);
                    if let Some(Item::Team { expanded: e, .. }) =
                        self.tabs[idx].data.items.get_mut(sel)
                    {
                        *e = false;
                    }
                } else {
                    // Expand — fetch (or reuse) channels, splice them in.
                    let team_id = team.id.clone();
                    let channels = if channels_loaded {
                        // We don't cache per-team channels separately
                        // in v0.1; always re-fetch on expand. Cheap.
                        self.graph.team_channels(&team_id)
                    } else {
                        self.graph.team_channels(&team_id)
                    };
                    match channels {
                        Ok(chs) => {
                            let new_rows: Vec<Item> = chs
                                .into_iter()
                                .map(|c| Item::Channel {
                                    team_id: team_id.clone(),
                                    channel: c,
                                })
                                .collect();
                            let after = sel + 1;
                            self.tabs[idx].data.items.splice(after..after, new_rows);
                            if let Some(Item::Team {
                                expanded: e,
                                channels_loaded: cl,
                                ..
                            }) = self.tabs[idx].data.items.get_mut(sel)
                            {
                                *e = true;
                                *cl = true;
                            }
                        }
                        Err(e) => self.status = format!("error: {e}"),
                    }
                }
            }
            Some(Item::Message(m)) => {
                // Search-hit message — open permalink.
                let url = crate::teams::permalink_for(&m);
                match webbrowser::open(&url) {
                    Ok(()) => self.status = format!("opened {url}"),
                    Err(e) => self.status = format!("open failed: {e}"),
                }
            }
            Some(Item::Channel { .. }) | Some(Item::Chat(_)) => {
                // Detail already loaded by refresh_detail_for_selection.
                self.refresh_detail_for_selection();
            }
            _ => {}
        }
    }

    /// `/` — start search query input (search tab only).
    pub fn start_search(&mut self) {
        let idx = self.active_tab;
        if self.tabs[idx].spec.kind != "search" {
            self.status = "switch to the search tab to query".into();
            return;
        }
        self.tabs[idx].data.search_mode = true;
        self.tabs[idx].data.search_query.clear();
        self.status = "search: type query, Enter to submit, Esc to cancel".into();
    }

    pub fn submit_search(&mut self) {
        let idx = self.active_tab;
        let q = self.tabs[idx].data.search_query.trim().to_string();
        self.tabs[idx].data.search_mode = false;
        if q.is_empty() {
            self.status = "(empty query — cancelled)".into();
            return;
        }
        self.status = format!("searching {q:?}…");
        match self.graph.search_messages(&q) {
            Ok(msgs) => {
                let count = msgs.len();
                self.tabs[idx].data.items = msgs.into_iter().map(Item::Message).collect();
                self.tabs[idx].data.selected = 0;
                self.tabs[idx].data.last_loaded = Some(Instant::now());
                self.status = format!("{count} hits for {q:?}");
                self.refresh_detail_for_selection();
            }
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    pub fn cancel_search(&mut self) {
        let idx = self.active_tab;
        self.tabs[idx].data.search_mode = false;
        self.tabs[idx].data.search_query.clear();
        self.status = "(search cancelled)".into();
    }

    /// `p` — start a post on a focused channel or chat.
    pub fn start_post(&mut self) {
        let target = match self.focused_item() {
            Some(Item::Channel { team_id, channel }) => Some(PostMode::Channel {
                team_id: team_id.clone(),
                channel_id: channel.id.clone(),
            }),
            Some(Item::Chat(c)) => Some(PostMode::Chat {
                chat_id: c.id.clone(),
            }),
            _ => None,
        };
        let Some(t) = target else {
            self.status = "`p` only works on a channel or chat".into();
            return;
        };
        self.post_mode = Some(t);
        self.post_buffer.clear();
        self.status = "post: type message, Ctrl+S send, Esc cancel".into();
    }

    /// `T` — threaded reply. v0.1: channels only. (Chat-level threads
    /// aren't a Graph concept; chat replies are just more messages.)
    pub fn start_thread_reply(&mut self) {
        let Some(m_in_chan) = self.focused_message_in_channel() else {
            self.status =
                "`T` only works on a message in a channel scrollback (focus a channel first)"
                    .into();
            return;
        };
        self.post_mode = Some(PostMode::ChannelReply {
            team_id: m_in_chan.team_id,
            channel_id: m_in_chan.channel_id,
            message_id: m_in_chan.message_id,
        });
        self.post_buffer.clear();
        self.status = "thread reply: type, Ctrl+S send, Esc cancel".into();
    }

    /// Look at the right-pane (detail) cursor — for v0.1 we don't
    /// support cursoring inside the detail pane, so this returns the
    /// first message in detail when the focused list item is a
    /// channel. Best-effort.
    fn focused_message_in_channel(&self) -> Option<ChannelMsgFocus> {
        let t = self.active();
        if let DetailKind::Channel(team_id, channel_id) = &t.data.detail_kind
            && let Some(m) = t.data.detail_messages.first()
        {
            return Some(ChannelMsgFocus {
                team_id: team_id.clone(),
                channel_id: channel_id.clone(),
                message_id: m.id.clone(),
            });
        }
        None
    }

    pub fn cancel_post(&mut self) {
        self.post_mode = None;
        self.post_buffer.clear();
        self.status = "(post cancelled)".into();
    }

    pub fn submit_post(&mut self) {
        let Some(mode) = self.post_mode.clone() else {
            return;
        };
        let text = self.post_buffer.trim().to_string();
        if text.is_empty() {
            self.status = "(empty — not sent)".into();
            self.post_mode = None;
            return;
        }
        let res = match &mode {
            PostMode::Channel {
                team_id,
                channel_id,
            } => self
                .graph
                .post_to_channel(team_id, channel_id, &text)
                .map(|_| ()),
            PostMode::Chat { chat_id } => self.graph.post_to_chat(chat_id, &text).map(|_| ()),
            PostMode::ChannelReply {
                team_id,
                channel_id,
                message_id,
            } => self
                .graph
                .reply_in_channel(team_id, channel_id, message_id, &text)
                .map(|_| ()),
        };
        match res {
            Ok(()) => {
                self.status = format!("posted ({} chars)", text.chars().count());
                self.post_mode = None;
                self.post_buffer.clear();
                // Refresh detail to show the new message.
                self.refresh_detail_for_selection();
            }
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    /// `R` — chat reaction picker (v0.1: short-press one of the 6 keys).
    /// Reactions are chat-only in Graph (`/me/chats/{cid}/messages/{mid}/setReaction`).
    pub fn react(&mut self, reaction: &str) {
        let t = self.active();
        let DetailKind::Chat(chat_id) = &t.data.detail_kind else {
            self.status = "`R` only works on a chat message (focus a chat first)".into();
            return;
        };
        let Some(msg) = t.data.detail_messages.first() else {
            self.status = "no message to react to".into();
            return;
        };
        let chat_id = chat_id.clone();
        let msg_id = msg.id.clone();
        match self.graph.set_chat_reaction(&chat_id, &msg_id, reaction) {
            Ok(()) => self.status = format!("reacted {reaction}"),
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    /// `y` — yank a permalink for the focused message or list item.
    pub fn yank(&mut self) {
        let payload = match self.focused_item() {
            Some(Item::Message(m)) => crate::teams::permalink_for(m),
            Some(Item::Channel { channel, .. }) => channel.web_url.clone().unwrap_or_default(),
            Some(Item::Chat(c)) => {
                // No Graph webUrl on chat directly — fall back to id.
                format!("chat:{}", c.id)
            }
            Some(Item::Team { team, .. }) => format!("team:{}", team.id),
            _ => String::new(),
        };
        if payload.is_empty() {
            // Try detail pane's first message.
            if let Some(m) = self.active().data.detail_messages.first() {
                let p = crate::teams::permalink_for(m);
                match crate::clipboard::copy(&p) {
                    Ok(()) => {
                        self.status = format!("copied permalink ({} chars)", p.chars().count())
                    }
                    Err(e) => self.status = format!("copy failed: {e}"),
                }
                return;
            }
            self.status = "nothing to copy".into();
            return;
        }
        let len = payload.chars().count();
        match crate::clipboard::copy(&payload) {
            Ok(()) => self.status = format!("copied ({len} chars)"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// Push a character into the active text buffer (search query or
    /// post buffer). Returns true if consumed.
    pub fn input_char(&mut self, c: char) -> bool {
        if self.post_mode.is_some() {
            self.post_buffer.push(c);
            return true;
        }
        let idx = self.active_tab;
        if self.tabs[idx].data.search_mode {
            self.tabs[idx].data.search_query.push(c);
            return true;
        }
        false
    }

    pub fn input_backspace(&mut self) -> bool {
        if self.post_mode.is_some() {
            self.post_buffer.pop();
            return true;
        }
        let idx = self.active_tab;
        if self.tabs[idx].data.search_mode {
            self.tabs[idx].data.search_query.pop();
            return true;
        }
        false
    }
}

struct ChannelMsgFocus {
    team_id: String,
    channel_id: String,
    message_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_spec_accepts_all_known_kinds() {
        for k in &["teams", "chats", "search", "threads"] {
            let t = Tab {
                name: "x".into(),
                kind: (*k).into(),
            };
            assert!(TabSpec::resolve(&t).is_ok(), "{k}");
        }
    }

    #[test]
    fn tab_spec_rejects_unknown() {
        let t = Tab {
            name: "x".into(),
            kind: "bogus".into(),
        };
        assert!(TabSpec::resolve(&t).is_err());
    }

    #[test]
    fn short_ts_extracts_md_hm() {
        assert_eq!(super::short_ts("2026-06-07T12:34:56.789Z"), "06-07 12:34");
    }
}
