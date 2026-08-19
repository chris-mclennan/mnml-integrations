//! App state — what's loaded, what's selected, the resolved JQL for
//! each configured tab. The UI layer reads from this.

use crate::config::{Config, ResolveMode, Tab, TabKind};
use crate::jira::{Client, Issue, IssueDetail, QuickFilter, Sprint, Transition};
use crate::tree::TreeState;
use anyhow::{Context, Result};
use ratatui::layout::Rect;
use std::collections::{BTreeSet, HashMap, HashSet};

pub struct App {
    pub cfg: Config,
    pub client: Client,
    /// One entry per `cfg.tabs`. Same order.
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    /// Toast/status line at the bottom of the screen.
    pub status: String,
    /// When true, render a right-half detail panel for the focused
    /// ticket. Toggled by `d`. Off by default — the table is wider.
    pub details_visible: bool,
    /// First-line offset into the detail body (vertical scroll within
    /// the right panel). Reset when the focused ticket changes.
    pub details_scroll: u16,
    /// Cache of `(issue.key, IssueDetail)` — populated on demand when
    /// the user focuses a ticket with the detail pane open. Survives
    /// tab switches; cleared per-key by an explicit refresh.
    pub detail_cache: HashMap<String, IssueDetail>,
    /// `Some(key)` while a detail-fetch is in flight, so we don't fire
    /// duplicate requests on rapid arrow-key navigation.
    pub detail_in_flight: Option<String>,
    /// Active client-side filter (substring match against key + summary,
    /// case-insensitive). Mode lifecycle:
    ///   `None`            → no filter; show all issues.
    ///   `Some(s)` + `editing == true` → user is typing; row count
    ///     updates live as `s` changes.
    ///   `Some(s)` + `editing == false` → filter committed; selection
    ///     navigates within the filtered subset; `n`/`N` jump matches.
    pub filter: Option<FilterState>,
    /// Status-transition overlay for the focused ticket. Opened by
    /// `t`. `Some` ⇒ greedy modal — keys go to the picker (digits to
    /// pick, ↑↓/jk to move, Enter / Esc to commit / cancel) instead
    /// of the list. Loaded lazily — `transitions` is `None` while
    /// the fetch is in flight.
    pub transition_picker: Option<TransitionPicker>,
    /// AccountId of the authenticated user, fetched once on first
    /// use (the unwatch DELETE endpoint requires it as a query
    /// param). `None` ⇒ not fetched yet; `Some(Err)` ⇒ permanent
    /// error (e.g. token revoked) — `w` no-ops with a status toast
    /// rather than retrying every keypress.
    pub my_account_id: Option<Result<String, String>>,
    /// Inline comment editor at the bottom of the detail panel. Opened
    /// by `c` when the detail panel is visible. Greedy modal — printable
    /// keys insert, Esc cancels, Ctrl+P posts. Multi-line via Enter.
    pub comment_editor: Option<CommentEditor>,
    /// Multi-row selection set — issue keys (not indices) so a refresh
    /// can't invalidate it. `Space` toggles the focused row; `t` and
    /// `w` operate on the whole set when non-empty, otherwise just the
    /// focused row. Cleared via Esc (after filter / detail / editor in
    /// the cascade).
    pub selection: BTreeSet<String>,
    /// `a` (assignee) / `f` (fixVersion) inline-edit modal. Holds the
    /// kind, the fetched item list, a substring filter, and a highlight
    /// position. Greedy modal — printable text refines the filter,
    /// Enter commits, Esc cancels.
    pub field_picker: Option<FieldPicker>,
    /// 2026-07-25 — set by main.rs when the sibling was launched
    /// with `--only`. Suppresses the top tab strip regardless of
    /// how many tabs remain after filtering — the caller (mnml's
    /// split Jira chips) has already picked the view for the user.
    pub hide_tab_strip: bool,
    /// 2026-08-07 — friendly-name cache for boards, keyed by id.
    /// Populated lazily on kanban render; used by the toolbar chip
    /// to show `[Board: HeliOS]` instead of `[Board:200]`.
    pub board_name_cache: HashMap<u64, String>,
    /// 2026-08-07 — mouse-rect registry, refreshed every frame by
    /// the UI layer. The event loop reads these to route clicks on
    /// kanban tabs (cards, toolbar chips, expand chevrons) since
    /// those don't fit the flat-row `table_row_at` model.
    pub rects: Rects,
    /// 2026-08-07 — per-column vertical scroll offset for the
    /// kanban board (order: To Do / In Progress / Testing / Done).
    /// j/k on the focused column or wheel scroll on hover moves it.
    pub kanban_col_scroll: [u16; 4],
    /// 2026-08-07 — cards the user has expanded inline (chevron
    /// click). Adds 2-3 rows of quick-peek info under the card.
    /// Deeper drill = the detail modal.
    pub kanban_expanded: HashSet<String>,
    /// 2026-08-07 — active card detail modal. `None` = closed.
    pub detail_modal: Option<DetailModal>,
}

/// Live rect registry, rebuilt every frame. Not persisted between
/// draws — the mouse handler just reads whatever the last draw left
/// behind. Keeping it on `App` (not `Frame`) so the event loop can
/// see it. Coordinates are absolute (terminal-space).
#[derive(Default)]
pub struct Rects {
    /// Kanban card body rects → issue index. Clicking a card body
    /// opens the detail modal.
    pub kanban_cards: Vec<(Rect, usize)>,
    /// Kanban expand-chevron rects → issue key. Toggles
    /// `kanban_expanded`.
    pub kanban_chevrons: Vec<(Rect, String)>,
    /// Kanban toolbar chip rects → chip kind. Clicking dispatches
    /// to the matching picker / cycles a filter.
    pub kanban_chips: Vec<(Rect, ChipKind)>,
    /// Kanban column body rects (whole column including header).
    /// Used by wheel-scroll on hover to know which column to scroll.
    pub kanban_cols: [Option<Rect>; 4],
    /// Detail modal's close button (`×`).
    pub modal_close: Option<Rect>,
    /// Show-more PR row rects → issue key. Bumps that key's
    /// `pr_show_count` by 3.
    pub pr_show_more: Vec<(Rect, String)>,
    /// 2026-08-18 (#991) — fix-version chip in the tree-table
    /// title bar. Click opens `open_tab_fix_version_picker` so the
    /// user doesn't need to remember the `f` keychord.
    pub version_chip: Option<Rect>,
}

/// 2026-08-07 — kanban toolbar chip kind. Each maps to an existing
/// action or a "coming soon" toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Board,
    /// 2026-08-17 (task #887) — Sprint chip. Only rendered on scrum
    /// boards whose sprint list is non-empty; hidden on kanban.
    /// Click opens the sprint picker.
    Sprint,
    Search,
    Team,
    Version,
    Epic,
    Type,
    Label,
    QuickFilters,
    /// 2026-08-17 (task #893) — opens the board's settings URL in the
    /// system browser. The Jira Cloud "Configure board" page.
    BoardSettings,
}

/// 2026-08-07 — card detail modal state. Loaded lazily — `data`
/// is `None` while the fetch is in flight. Scroll applies to the
/// right (long-text) pane; left column is always compact.
pub struct DetailModal {
    /// Issue key the modal is bound to.
    pub key: String,
    /// Full raw JSON returned by `/rest/api/3/issue/{key}?fields=…`.
    /// `None` while loading; renderer shows a spinner.
    pub data: Option<serde_json::Value>,
    /// Vertical scroll into the description / long-text pane.
    pub scroll: u16,
    /// Populated on request failure so we can show an error banner
    /// inside the modal instead of blowing away the status line.
    pub error: Option<String>,
}

/// Inline-edit picker for one of two fields. The mechanics are
/// identical (item list, filter, highlight, Enter to commit) — only
/// the source data + the commit handler differ.
#[derive(Debug, Clone)]
pub struct FieldPicker {
    pub kind: FieldKind,
    /// `(id, label)` tuples. For Assignee, id = accountId, label =
    /// display name. For FixVersion, id = name (Jira accepts versions
    /// by name for the PUT), label = name.
    pub items: Vec<(String, String)>,
    /// `None` while the fetch is in flight; `Some(Vec)` once loaded.
    pub loaded: bool,
    pub filter: String,
    pub cursor: usize,
    pub selected: usize,
    pub error: Option<String>,
    /// 2026-08-17 (task #893) — multi-select state used by the
    /// QuickFilter picker. `Some(set)` ⇒ picker renders `[x]`/`[ ]`
    /// prefixes, Space toggles the row's id in the set, Enter closes
    /// and commits the whole set. `None` ⇒ classic single-select.
    /// The Sprint picker deliberately stays single-select (a board
    /// is on exactly one sprint at a time).
    pub multi_selected: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Assignee,
    FixVersion,
    /// 2026-08-06 — Team filter picker for board tabs. Items come
    /// from the union of `components` + `labels` across the current
    /// tab's issues (no Jira roundtrip needed). Commit writes back
    /// to `tab.team` in-memory + re-buckets the kanban immediately;
    /// persistence to config.toml is a follow-up.
    Team,
    /// 2026-08-06 — Fix Versions tab-view picker. Same source data as
    /// `FixVersion` (project versions from Jira), but committing
    /// rewrites `tab.jql` to filter the whole tab to that version
    /// instead of assigning the picked version to the focused ticket.
    /// Bound to `V` (capital) on fix_version_tree tabs.
    TabFixVersion,
    /// 2026-08-07 — Actions picker for the focused ticket. Items are
    /// `dispatch::buttons_for_ticket` (Implement / Fix / Triage).
    /// Bound to `.` on any tab. Commit fires
    /// `dispatch_ticket_action` — same handoff the on-card buttons
    /// use. Mouse users click the buttons directly; keyboard users
    /// press `.` to pick.
    TicketAction,
    /// 2026-08-17 (task #887) — Sprint picker for board tabs. Items
    /// come from `TabState.sprints_cache`, sorted current-active
    /// first then future then last-N-closed. Commit sets
    /// `TabState.selected_sprint_id` and refetches the board via
    /// `?jql=... AND sprint = <id>`.
    Sprint,
    /// 2026-08-17 (task #893) — Quick filter picker for board tabs.
    /// Multi-select — Space toggles the row under the cursor, Enter
    /// closes + applies (writes `TabState.active_quick_filter_ids`
    /// and refetches).
    QuickFilter,
    /// #1004 (2026-08-18) — Issue Type picker for board tabs. Items
    /// are the union of issuetype.name across the tab's issues (no
    /// Jira roundtrip). Commit writes `tab.issue_type` in-memory and
    /// re-buckets on next paint.
    IssueType,
    /// #1004 (2026-08-18) — Label picker for board tabs. Items are
    /// the union of labels across the tab's issues. Commit writes
    /// `tab.label` in-memory.
    Label,
}

impl FieldPicker {
    /// Indices of `items` matching the current filter (case-insensitive
    /// substring against label). Empty filter ⇒ all items.
    pub fn visible_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let needle = self.filter.to_ascii_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(i, (_, label))| label.to_ascii_lowercase().contains(&needle).then_some(i))
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommentEditor {
    /// Issue key the comment will be posted against (captured at open).
    pub key: String,
    pub buffer: String,
    pub cursor: usize,
    /// `Some(msg)` while posting; suppresses further key input and is
    /// displayed in the editor's status row.
    pub posting: bool,
    pub error: Option<String>,
}

/// Modal state for the `t` transition picker.
#[derive(Debug, Clone)]
pub struct TransitionPicker {
    /// Issue key the picker is bound to (captured at open time so a
    /// background list refresh / cursor move doesn't change targets
    /// mid-pick).
    pub key: String,
    /// `None` while the GET is in flight; `Some(Vec)` once loaded.
    /// Empty vec is a legitimate response — no transitions available.
    pub transitions: Option<Vec<Transition>>,
    /// Highlighted row in the picker (0-based).
    pub selected: usize,
    /// Most recent error message — surfaced inside the overlay rather
    /// than blowing away the status line.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FilterState {
    pub buffer: String,
    pub cursor: usize,
    /// True while `/` is open and the user hasn't hit Enter / Esc yet.
    pub editing: bool,
}

pub struct TabState {
    pub name: String,
    /// Final JQL after any release auto-resolution.
    pub jql: String,
    pub issues: Vec<Issue>,
    /// Cursor position. Semantics depend on tree state:
    ///   - `tree.is_none()` (flat tabs): index into `issues[]`.
    ///   - `tree.is_some()` (FixVersionTree tabs): index into
    ///     `visible_rows(cfg, tab_cfg)` — a mixed list of
    ///     GroupHeader / Ticket / LinkedPr / etc.
    /// Callers use the type-directed helpers on TabState below
    /// rather than reading `.selected` raw.
    pub selected: usize,
    /// Wall-clock time of the most recent successful fetch.
    pub last_fetched: Option<std::time::Instant>,
    pub last_error: Option<String>,
    /// 2026-07-25 — populated for FixVersionTree kind. Layers
    /// status grouping + per-ticket expansion on top of `issues`.
    /// `None` on legacy / non-tree tabs — renderer falls back to
    /// the flat table path in that case.
    pub tree: Option<TreeState>,
    /// 2026-08-17 (task #887) — board-tab-only. Overrides which
    /// sprint the board fetch scopes to. `None` = default behavior
    /// (the Agile API's `/board/{id}/issue` returns whatever the
    /// board's own view shows, which is the active sprint on scrum
    /// boards). `Some(id)` = additionally AND `sprint = <id>` into
    /// the fetch, letting the user peek at upcoming / previous
    /// sprints without changing the board itself.
    ///
    /// In-memory only — not written back to config. On restart the
    /// tab reverts to the board's default view.
    pub selected_sprint_id: Option<u64>,
    /// 2026-08-17 (task #887) — lazy-loaded sprint list for the
    /// tab's `board_id`. Populated on first sprint-picker open;
    /// re-used across subsequent opens until refresh. `None` = never
    /// fetched; `Some(vec)` = fetched (empty vec = kanban board).
    pub sprints_cache: Option<Vec<Sprint>>,
    /// 2026-08-17 (task #893) — active quick-filter ids. Each id
    /// contributes an `AND (<qf.jql>)` clause to the board fetch.
    /// Multiple can be active simultaneously; empty set = no
    /// quick-filter narrowing. Persists across refreshes but not
    /// across restarts (in-memory only).
    pub active_quick_filter_ids: BTreeSet<u64>,
    /// 2026-08-17 (task #893) — lazy-loaded quick-filter list for
    /// the tab's `board_id`. Same lifecycle as `sprints_cache`.
    pub quick_filters_cache: Option<Vec<QuickFilter>>,
}

impl TabState {
    /// 2026-07-25 — for FixVersionTree tabs, compute the current
    /// visible-row list (headers + tickets + PR sub-rows). Applies
    /// grouping + bumps + expansion state. `None` for non-tree
    /// tabs; caller uses the flat `visible_indices` path instead.
    pub fn tree_rows(
        &self,
        tab_cfg: &crate::config::Tab,
        cfg: &crate::config::Config,
    ) -> Option<Vec<crate::tree::VisibleRow>> {
        let tree = self.tree.as_ref()?;
        let mut rows = crate::tree::compute_visible_rows(&self.issues, tree, tab_cfg, cfg);
        crate::tree::splice_ticket_sub_rows(&mut rows, &self.issues, tree);
        Some(rows)
    }
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        let mut tabs: Vec<TabState> = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let jql = resolve_tab_jql(t, &client).await.unwrap_or_else(|e| {
                // Fall back to a placeholder JQL that yields zero
                // results so the tab is still present; the error
                // surfaces in the per-tab last_error.
                eprintln!("tab '{}': resolve failed: {e}", t.name);
                "issuekey = ''".to_string()
            });
            // 2026-08-06 — extend TreeState to work_* + board_*
            // kinds too. Was: FixVersionTree only. User request:
            // work_assigned should render like fix_version_tree
            // (status-grouped headers, per-ticket expand to show
            // linked PRs). All the tree machinery already exists;
            // the only reason work tabs were flat was this
            // allocation. Legacy no-kind tabs keep the flat table.
            let tree = if matches!(
                t.kind,
                Some(TabKind::FixVersionTree)
                    | Some(TabKind::WorkAssigned)
                    | Some(TabKind::WorkRecentlyDone)
                    | Some(TabKind::WorkRecent)
                    | Some(TabKind::BoardActiveSprint)
                    | Some(TabKind::BoardBacklog)
            ) {
                Some(TreeState::default())
            } else {
                None
            };
            tabs.push(TabState {
                name: t.name.clone(),
                jql,
                issues: Vec::new(),
                selected: 0,
                last_fetched: None,
                last_error: None,
                tree,
                selected_sprint_id: None,
                sprints_cache: None,
                active_quick_filter_ids: BTreeSet::new(),
                quick_filters_cache: None,
            });
        }
        let mut app = App {
            cfg,
            client,
            tabs,
            active_tab: 0,
            status: String::new(),
            details_visible: false,
            details_scroll: 0,
            detail_cache: HashMap::new(),
            detail_in_flight: None,
            filter: None,
            transition_picker: None,
            my_account_id: None,
            comment_editor: None,
            selection: BTreeSet::new(),
            field_picker: None,
            hide_tab_strip: false,
            board_name_cache: HashMap::new(),
            rects: Rects::default(),
            kanban_col_scroll: [0; 4],
            kanban_expanded: HashSet::new(),
            detail_modal: None,
        };
        // 2026-07-25 — previously did a /myself pre-flight to
        // detect stale tokens (which /search/jql silently
        // masks as empty results). Reverted because scoped API
        // tokens routinely lack the `read:me` scope /myself
        // requires — a valid scoped token that CAN search will
        // 401 on /myself, triggering a false "auth failed"
        // panel-wide error. Real auth failures still surface via
        // per-tab search errors (401 does propagate through
        // /search/jql on a fully-revoked token — the empty-
        // results quirk is only for scoped-but-wrong-context).
        app.refresh_active().await;
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
            // Re-fetch if we've never loaded this tab.
            let needs = self.tabs[idx].last_fetched.is_none();
            if needs {
                self.status = format!("loading {}…", self.tabs[idx].name);
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        // 2026-07-25 — tree tabs (FixVersionTree) treat `selected`
        // as an index into the mixed VisibleRow list, not into
        // `issues[]`. Nav clamps against that list's length so
        // Up/Down move by exactly one rendered row (header,
        // ticket, or PR sub-row).
        if self.active().tree.is_some() {
            let Some(tab_cfg) = self.cfg.tabs.get(self.active_tab) else {
                return;
            };
            let rows = self
                .active()
                .tree_rows(tab_cfg, &self.cfg)
                .unwrap_or_default();
            if rows.is_empty() {
                return;
            }
            let cur = self.active().selected as isize;
            let new = (cur + delta).clamp(0, rows.len() as isize - 1) as usize;
            self.active_mut().selected = new;
            return;
        }
        // Flat tab path: step through visible_indices() so a filter
        // doesn't strand the selection on a hidden row.
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let cur = self.active().selected;
        let pos = visible.iter().position(|&i| i == cur).unwrap_or(0) as isize;
        let new_pos = (pos + delta).clamp(0, visible.len() as isize - 1) as usize;
        self.active_mut().selected = visible[new_pos];
    }

    /// Re-fetch the active tab's issues. Updates `last_fetched` and
    /// `last_error` on the tab.
    pub async fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let base_jql = self.tabs[idx].jql.clone();
        // 2026-08-07 — push the team filter into JQL server-side. Was:
        // fetched up to 100 tickets, then filtered client-side by
        // team. Sprints with 100+ tickets across teams meant the
        // selected team's tickets could be past the cap and never
        // rendered. User: "HeliOS is missing some things."
        let team = self
            .cfg
            .tabs
            .get(idx)
            .and_then(|t| t.team.clone())
            .filter(|s| !s.trim().is_empty());
        let jql = match &team {
            Some(t) => {
                let escaped = t.replace('"', "\\\"");
                let (where_part, order_part) = split_order_by(&base_jql);
                // Include the configured team custom-field clause
                // when the user set one. JQL accepts either the
                // customfield_XXXXX id or the display name in quotes;
                // we prefer the display name for legibility, falling
                // back to the id.
                let mut clauses = Vec::new();
                if let Some(field_name) = self
                    .cfg
                    .team_field_name
                    .as_ref()
                    .filter(|s| !s.trim().is_empty())
                    .or(self
                        .cfg
                        .team_field_id
                        .as_ref()
                        .filter(|s| !s.trim().is_empty()))
                {
                    clauses.push(format!("\"{field_name}\" = \"{escaped}\""));
                }
                clauses.push(format!("component = \"{escaped}\""));
                clauses.push(format!("labels = \"{escaped}\""));
                let team_clause = format!("({})", clauses.join(" OR "));
                let mut out = format!("({where_part}) AND {team_clause}");
                if !order_part.is_empty() {
                    out.push(' ');
                    out.push_str(&order_part);
                }
                out
            }
            None => base_jql,
        };
        // Include the configured team custom-field id in the response
        // fields so `team_value_of` can read it back off the issue.
        let extra_fields: Vec<String> = self
            .cfg
            .team_field_id
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| vec![s.clone()])
            .unwrap_or_default();
        // 2026-08-07 — board_id-backed mode. When the tab has
        // `board_id = N`, fetch what Jira's own UI shows for that
        // board (respecting its saved filter + active sprint) via
        // the Agile REST API. Team filter still layers on top as
        // an extra `?jql=...` clause. Overrides synthetic JQL.
        self.status = format!("refreshing {}…", self.tabs[idx].name);
        let board_id = self.cfg.tabs.get(idx).and_then(|t| t.board_id);
        let fetch_result = match board_id {
            Some(id) => {
                // Compose the `?jql=<extra>` clauses that layer on top
                // of what the board itself already filters by:
                //   - team clause (component / labels / configured team
                //     custom field)
                //   - sprint override (task #887)
                //   - active quick filters (task #893)
                // All AND'd together; empty ⇒ no extra ?jql= param.
                let mut extra_clauses: Vec<String> = Vec::new();
                if let Some(t) = team.as_ref() {
                    let escaped = t.replace('"', "\\\"");
                    let mut clauses = Vec::new();
                    if let Some(field_name) = self
                        .cfg
                        .team_field_name
                        .as_ref()
                        .filter(|s| !s.trim().is_empty())
                        .or(self
                            .cfg
                            .team_field_id
                            .as_ref()
                            .filter(|s| !s.trim().is_empty()))
                    {
                        clauses.push(format!("\"{field_name}\" = \"{escaped}\""));
                    }
                    clauses.push(format!("component = \"{escaped}\""));
                    clauses.push(format!("labels = \"{escaped}\""));
                    extra_clauses.push(format!("({})", clauses.join(" OR ")));
                }
                // Sprint override — bare `sprint = <id>` clause. The
                // Agile /board/{id}/issue endpoint accepts JQL against
                // the sprint field even though the board's own view
                // already implies "in the active sprint" — the extra
                // clause is what lets us pin to a specific sprint id
                // (past or future).
                if let Some(sprint_id) = self.tabs[idx].selected_sprint_id {
                    extra_clauses.push(format!("sprint = {sprint_id}"));
                }
                // Quick filters — each contributes a bracketed sub-
                // clause of its own JQL. We wrap in parens defensively
                // in case the quick filter contains a top-level OR.
                if !self.tabs[idx].active_quick_filter_ids.is_empty()
                    && let Some(cache) = self.tabs[idx].quick_filters_cache.as_ref()
                {
                    for qf in cache {
                        if self.tabs[idx].active_quick_filter_ids.contains(&qf.id)
                            && !qf.jql.trim().is_empty()
                        {
                            extra_clauses.push(format!("({})", qf.jql.trim()));
                        }
                    }
                }
                let extra_jql = if extra_clauses.is_empty() {
                    None
                } else {
                    Some(extra_clauses.join(" AND "))
                };
                self.client
                    .fetch_board_issues(id, extra_jql.as_deref(), &extra_fields)
                    .await
            }
            None => self.client.search(&jql, 100, &extra_fields).await,
        };
        match fetch_result {
            Ok(issues) => {
                self.tabs[idx].issues = issues;
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx]
                    .selected
                    .min(self.tabs[idx].issues.len().saturating_sub(1));
                self.status = format!(
                    "{} · {} issues",
                    self.tabs[idx].name,
                    self.tabs[idx].issues.len()
                );
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Open the focused ticket in the OS default browser.
    pub fn open_focused(&mut self) {
        let Some(issue) = self.active().issues.get(self.active().selected) else {
            return;
        };
        let url = self.client.issue_url(&issue.key);
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {} in browser", issue.key),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// Toggle the right-half ticket detail panel. On first show, kicks
    /// off a detail fetch for the focused ticket (if not already cached).
    pub async fn toggle_details(&mut self) {
        self.details_visible = !self.details_visible;
        self.details_scroll = 0;
        if self.details_visible {
            self.ensure_focused_detail().await;
        }
    }

    /// Issue key of the currently-focused ticket, or `None` if the
    /// active tab is empty.
    pub fn focused_key(&self) -> Option<String> {
        self.active()
            .issues
            .get(self.active().selected)
            .map(|i| i.key.clone())
    }

    /// Borrow the detail for the focused ticket, if cached.
    pub fn focused_detail(&self) -> Option<&IssueDetail> {
        let key = self
            .active()
            .issues
            .get(self.active().selected)?
            .key
            .clone();
        self.detail_cache.get(&key)
    }

    /// Fetch the focused ticket's description + comments if we don't
    /// already have them cached. No-op when the focused row is empty
    /// or another fetch is in flight.
    pub async fn ensure_focused_detail(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        if self.detail_cache.contains_key(&key) {
            return;
        }
        if self.detail_in_flight.as_deref() == Some(&key) {
            return;
        }
        self.detail_in_flight = Some(key.clone());
        match self.client.fetch_issue_detail(&key).await {
            Ok(detail) => {
                self.detail_cache.insert(key, detail);
            }
            Err(e) => {
                // Park an error placeholder so we don't refetch on
                // every key event. User-facing message in the status
                // line.
                self.status = format!("detail fetch failed for {key}: {e}");
                self.detail_cache.insert(key, IssueDetail::default());
            }
        }
        self.detail_in_flight = None;
    }

    /// Drop the cached detail for the focused ticket so the next
    /// `ensure_focused_detail` call re-fetches. Used by `r` when the
    /// detail panel is visible — the list refresh would otherwise
    /// leave stale narrative content.
    pub fn invalidate_focused_detail(&mut self) {
        if let Some(key) = self.focused_key() {
            self.detail_cache.remove(&key);
        }
    }

    /// 2026-07-25 — return the VisibleRow under the cursor on a
    /// tree tab. `None` for non-tree tabs or an out-of-range
    /// cursor.
    pub fn focused_tree_row(&self) -> Option<crate::tree::VisibleRow> {
        let tab_cfg = self.cfg.tabs.get(self.active_tab)?;
        let rows = self.active().tree_rows(tab_cfg, &self.cfg)?;
        rows.get(self.active().selected).cloned()
    }

    /// 2026-07-25 — Space/Enter dispatch entry point for tree
    /// tabs. Routes by the row variant under the cursor:
    ///   GroupHeader → toggle collapsed_groups[status]
    ///   Ticket      → toggle expanded_tickets[key]; on expand,
    ///                 kick off the linked-PR fetch.
    ///   LinkedPr    → open the PR URL in browser (Phase 5
    ///                 wires the [ Review ] button separately).
    ///   PrLoading / PrEmpty → no-op.
    /// Falls through to the flat-table Enter behavior when the
    /// active tab isn't a tree tab.
    pub async fn tree_activate_focused(&mut self) {
        use crate::tree::VisibleRow;
        let Some(row) = self.focused_tree_row() else {
            self.open_focused();
            return;
        };
        match row {
            VisibleRow::GroupHeader { status, .. } => {
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    if !tree.collapsed_groups.insert(status.clone()) {
                        tree.collapsed_groups.remove(&status);
                    }
                }
            }
            VisibleRow::Ticket { issue_idx, .. } => {
                let key = self.active().issues[issue_idx].key.clone();
                let was_expanded = self
                    .active()
                    .tree
                    .as_ref()
                    .is_some_and(|t| t.expanded_tickets.contains(&key));
                if was_expanded {
                    if let Some(tree) = self.active_mut().tree.as_mut() {
                        tree.expanded_tickets.remove(&key);
                    }
                } else {
                    if let Some(tree) = self.active_mut().tree.as_mut() {
                        tree.expanded_tickets.insert(key.clone());
                    }
                    // Trigger fetch on first expand. Idempotent —
                    // no-op if already cached.
                    self.ensure_ticket_prs(&key).await;
                }
            }
            VisibleRow::LinkedPr { issue_idx, pr_idx } => {
                let key = self.active().issues[issue_idx].key.clone();
                if let Some(url) = self
                    .active()
                    .tree
                    .as_ref()
                    .and_then(|t| t.pr_cache.get(&key))
                    .and_then(|prs| prs.get(pr_idx))
                    .map(|pr| pr.url.clone())
                    && !url.is_empty()
                {
                    let _ = webbrowser::open(&url);
                    self.status = format!("opened {url}");
                }
            }
            VisibleRow::PrShowMore { issue_idx, .. } => {
                // Enter/Space on a "show more" row reveals the next
                // batch of PRs. Same as clicking it.
                let key = self.active().issues[issue_idx].key.clone();
                self.pr_show_more(&key);
            }
            VisibleRow::PrLoading { .. }
            | VisibleRow::PrEmpty { .. }
            | VisibleRow::PrPipelineLoading { .. }
            | VisibleRow::PrPipelineEmpty { .. }
            | VisibleRow::PrPipelineError { .. }
            | VisibleRow::PrPipeline { .. } => {}
        }
    }

    /// 2026-07-26 — Right/l on a tree tab. Expand-only: opens a
    /// collapsed group / ticket / merged linked-PR. No-op if already
    /// expanded (Left / TreeCollapse handles the reverse). Fires the
    /// linked-PR fetch on ticket first-expand + the pipeline fetch on
    /// LinkedPr first-expand.
    ///
    /// LinkedPr row: only expandable when the PR's status is
    /// MERGED (open / draft PRs don't have a merge-commit pipeline
    /// yet). Right on an OPEN/DRAFT PR is a no-op.
    pub async fn tree_expand_focused(&mut self) {
        use crate::tree::VisibleRow;
        let Some(row) = self.focused_tree_row() else {
            return;
        };
        match row {
            VisibleRow::GroupHeader {
                status, expanded, ..
            } => {
                if !expanded && let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.collapsed_groups.remove(&status);
                }
            }
            VisibleRow::Ticket { issue_idx, .. } => {
                let key = self.active().issues[issue_idx].key.clone();
                let already = self
                    .active()
                    .tree
                    .as_ref()
                    .is_some_and(|t| t.expanded_tickets.contains(&key));
                if !already {
                    if let Some(tree) = self.active_mut().tree.as_mut() {
                        tree.expanded_tickets.insert(key.clone());
                    }
                    self.ensure_ticket_prs(&key).await;
                }
            }
            VisibleRow::LinkedPr { issue_idx, pr_idx } => {
                let key = self.active().issues[issue_idx].key.clone();
                // Pull PR id, url, merged-state from the cache.
                let Some((pr_id, pr_url, is_merged)) = self
                    .active()
                    .tree
                    .as_ref()
                    .and_then(|t| t.pr_cache.get(&key))
                    .and_then(|prs| prs.get(pr_idx))
                    .map(|pr| {
                        (
                            pr.id.clone(),
                            pr.url.clone(),
                            pr.status.eq_ignore_ascii_case("MERGED"),
                        )
                    })
                else {
                    return;
                };
                // Only merged PRs have a post-merge pipeline to
                // drill into. Open/draft PRs render as terminal
                // (no chevron) — Right is a no-op.
                if !is_merged {
                    return;
                }
                let pkey = (key.clone(), pr_id.clone());
                let already = self
                    .active()
                    .tree
                    .as_ref()
                    .is_some_and(|t| t.expanded_prs.contains(&pkey));
                if !already {
                    if let Some(tree) = self.active_mut().tree.as_mut() {
                        tree.expanded_prs.insert(pkey.clone());
                    }
                    self.ensure_pr_pipelines(&key, &pr_id, &pr_url).await;
                }
            }
            _ => {}
        }
    }

    /// 2026-07-26 — Left/h on a tree tab. Collapse-only: closes
    /// an expanded group / ticket / linked-PR. When focus is on a
    /// pipeline sub-row (or a "loading" / "no linked PRs" hint),
    /// collapse the parent (PR or ticket, one level up). No async
    /// — no fetch needed.
    pub fn tree_collapse_focused(&mut self) {
        use crate::tree::VisibleRow;
        let Some(row) = self.focused_tree_row() else {
            return;
        };
        match row {
            VisibleRow::GroupHeader {
                status, expanded, ..
            } => {
                if expanded && let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.collapsed_groups.insert(status);
                }
            }
            VisibleRow::Ticket { issue_idx, .. } => {
                let key = self.active().issues[issue_idx].key.clone();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.expanded_tickets.remove(&key);
                }
            }
            VisibleRow::LinkedPr { issue_idx, pr_idx } => {
                // On a LinkedPr row — Left collapses the PR (if
                // expanded), else falls through to collapsing the
                // parent ticket (matches VS Code-style tree nav).
                let key = self.active().issues[issue_idx].key.clone();
                let pr_id = self
                    .active()
                    .tree
                    .as_ref()
                    .and_then(|t| t.pr_cache.get(&key))
                    .and_then(|prs| prs.get(pr_idx))
                    .map(|pr| pr.id.clone());
                if let (Some(pr_id), Some(tree)) = (pr_id, self.active_mut().tree.as_mut()) {
                    let pkey = (key.clone(), pr_id);
                    if tree.expanded_prs.remove(&pkey) {
                        return;
                    }
                    // Not expanded ⇒ collapse the parent ticket.
                    tree.expanded_tickets.remove(&key);
                }
            }
            VisibleRow::PrLoading { issue_idx } | VisibleRow::PrEmpty { issue_idx } => {
                // Ticket-level hint rows: collapse the ticket.
                let key = self.active().issues[issue_idx].key.clone();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.expanded_tickets.remove(&key);
                }
            }
            VisibleRow::PrShowMore { issue_idx, .. } => {
                // On a "show more" row: collapse the parent ticket
                // (same as Left on any PR sub-row).
                let key = self.active().issues[issue_idx].key.clone();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.expanded_tickets.remove(&key);
                }
            }
            VisibleRow::PrPipelineLoading { issue_idx, pr_idx }
            | VisibleRow::PrPipelineEmpty { issue_idx, pr_idx }
            | VisibleRow::PrPipelineError { issue_idx, pr_idx }
            | VisibleRow::PrPipeline {
                issue_idx, pr_idx, ..
            } => {
                // On a pipeline sub-row: collapse the parent PR
                // (removes the entry from expanded_prs so the
                // pipeline sub-rows disappear on next render).
                let key = self.active().issues[issue_idx].key.clone();
                let pr_id = self
                    .active()
                    .tree
                    .as_ref()
                    .and_then(|t| t.pr_cache.get(&key))
                    .and_then(|prs| prs.get(pr_idx))
                    .map(|pr| pr.id.clone());
                if let (Some(pr_id), Some(tree)) = (pr_id, self.active_mut().tree.as_mut()) {
                    tree.expanded_prs.remove(&(key, pr_id));
                }
            }
        }
    }

    /// 2026-07-25 — dispatch a ticket-level action (Implement /
    /// Fix / Triage) for the focused ticket. Uses the standard
    /// the configured dispatch_workspace paths for queue + IPC. No-op on
    /// non-tree tabs or when the cursor isn't on a Ticket row.
    ///
    /// `kind` should be one of "implement", "fix", "triage" —
    /// matches `TicketButton::kind()`.
    pub fn dispatch_ticket_action(&mut self, kind: &str) {
        let Some(crate::tree::VisibleRow::Ticket { issue_idx, .. }) = self.focused_tree_row()
        else {
            self.status = "no ticket under cursor".to_string();
            return;
        };
        let issue = self.active().issues[issue_idx].clone();
        let jira_url = self.client.issue_url(&issue.key);
        let d = crate::dispatch::Dispatch::for_ticket(kind, &issue, jira_url);
        let (queue_dir, ipc_dir) =
            crate::dispatch::workspace_dispatch_paths(self.cfg.dispatch_workspace.as_deref());
        self.status = crate::dispatch::dispatch(&d, queue_dir.as_deref(), ipc_dir.as_deref());
    }

    /// 2026-07-25 — dispatch a Review action for the focused
    /// LinkedPr row. No-op when the cursor isn't on a PR row.
    pub fn dispatch_review_focused_pr(&mut self) {
        let Some(crate::tree::VisibleRow::LinkedPr { issue_idx, pr_idx }) = self.focused_tree_row()
        else {
            self.status = "no PR under cursor".to_string();
            return;
        };
        let issue = self.active().issues[issue_idx].clone();
        let key = issue.key.clone();
        let Some(pr_url) = self
            .active()
            .tree
            .as_ref()
            .and_then(|t| t.pr_cache.get(&key))
            .and_then(|prs| prs.get(pr_idx))
            .map(|pr| pr.url.clone())
        else {
            self.status = "PR has no URL".to_string();
            return;
        };
        let jira_url = self.client.issue_url(&issue.key);
        let d = crate::dispatch::Dispatch::for_pr(&issue, jira_url, pr_url);
        let (queue_dir, ipc_dir) =
            crate::dispatch::workspace_dispatch_paths(self.cfg.dispatch_workspace.as_deref());
        self.status = crate::dispatch::dispatch(&d, queue_dir.as_deref(), ipc_dir.as_deref());
    }

    /// 2026-07-25 — fetch linked PRs for `issue_key` via the Jira
    /// dev-status API and cache them on the active tab's tree.
    /// No-op when:
    ///   - the active tab isn't a FixVersionTree (no tree state);
    ///   - the key isn't in the tab's `issues` list (nothing to
    ///     look up an `issue.id` for);
    ///   - the PR cache already has an entry (avoid re-fetch spam).
    /// Errors surface on `self.status` and leave the cache absent
    /// so a subsequent explicit refresh can retry.
    pub async fn ensure_ticket_prs(&mut self, issue_key: &str) {
        // Guard: tree tab only.
        if self.active().tree.is_none() {
            return;
        }
        // Guard: cache already populated (Some(empty) counts as
        // "fetched, no PRs" — don't refetch).
        if self
            .active()
            .tree
            .as_ref()
            .is_some_and(|t| t.pr_cache.contains_key(issue_key))
        {
            return;
        }
        // Resolve issue.id from the flat list.
        let Some(issue_id) = self
            .active()
            .issues
            .iter()
            .find(|i| i.key == issue_key)
            .map(|i| i.id.clone())
            .filter(|id| !id.is_empty())
        else {
            self.status = format!("{issue_key}: no numeric id (was it re-fetched?)");
            return;
        };
        // Bitbucket connector; other tenants
        // can extend this to try github/azure fallback if needed.
        match self.client.list_prs_for_issue(&issue_id, "bitbucket").await {
            Ok(prs) => {
                let n = prs.len();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.pr_cache.insert(issue_key.to_string(), prs);
                }
                self.status = format!("{issue_key}: {n} linked PR(s)");
            }
            Err(e) => {
                self.status = format!("{issue_key}: linked-PR fetch failed: {e}");
                // Leave cache absent so `r` on the ticket retries.
            }
        }
    }

    /// Fetch post-merge Bitbucket pipelines for `pr_url` and cache
    /// them on the active tab's tree under `(issue_key, pr_id)`.
    /// Mirrors [`Self::ensure_ticket_prs`] shape: no-op on non-tree
    /// tabs, on a cache hit, or on an existing terminal error entry.
    /// On failure records the error message in `pipeline_errors` so
    /// the UI renders "pipeline lookup failed: <msg>" instead of
    /// spinning forever.
    pub async fn ensure_pr_pipelines(&mut self, issue_key: &str, pr_id: &str, pr_url: &str) {
        // Guard: tree tab only.
        if self.active().tree.is_none() {
            return;
        }
        let cache_key = (issue_key.to_string(), pr_id.to_string());
        // Guard: already resolved (cache hit or error).
        if self.active().tree.as_ref().is_some_and(|t| {
            t.pipeline_cache.contains_key(&cache_key) || t.pipeline_errors.contains_key(&cache_key)
        }) {
            return;
        }
        // Lazy — a new bitbucket::Client per fetch. `from_env`
        // reads BITBUCKET_ACCESS_TOKEN each call; cheap enough that
        // caching the client isn't worth threading through app
        // state. Missing token surfaces cleanly as a pipeline_error.
        let client = match crate::bitbucket::Client::from_env() {
            Ok(c) => c,
            Err(e) => {
                let msg = e.to_string();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.pipeline_errors.insert(cache_key, msg.clone());
                }
                self.status = format!("{issue_key} {pr_id}: {msg}");
                return;
            }
        };
        self.status = format!("fetching pipeline for {issue_key} {pr_id}…");
        match client.fetch_pipelines_for_pr_url(pr_url).await {
            Ok(pipelines) => {
                let n = pipelines.len();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.pipeline_cache.insert(cache_key, pipelines);
                }
                self.status = format!("{issue_key} {pr_id}: {n} pipeline(s) on merge commit");
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(tree) = self.active_mut().tree.as_mut() {
                    tree.pipeline_errors.insert(cache_key, msg.clone());
                }
                self.status = format!("{issue_key} {pr_id} pipeline lookup: {msg}");
            }
        }
    }

    /// Open the `/` filter editor. Pre-loads with whatever's already
    /// committed (so re-pressing `/` lets you refine an existing
    /// filter without retyping). Cursor at end.
    pub fn open_filter(&mut self) {
        let initial = self
            .filter
            .as_ref()
            .map(|f| f.buffer.clone())
            .unwrap_or_default();
        let cursor = initial.chars().count();
        self.filter = Some(FilterState {
            buffer: initial,
            cursor,
            editing: true,
        });
    }

    /// Close the `/` filter editor. Mode picks whether to keep what's
    /// typed (`Commit` — Enter) or drop it entirely (`Cancel` — Esc on
    /// an empty filter, or two Esc's). An empty committed buffer is
    /// treated as "no filter".
    pub fn close_filter(&mut self, mode: FilterClose) {
        let Some(state) = self.filter.as_mut() else {
            return;
        };
        match mode {
            FilterClose::Commit => {
                if state.buffer.trim().is_empty() {
                    self.filter = None;
                } else {
                    state.editing = false;
                }
            }
            FilterClose::Cancel => {
                self.filter = None;
            }
        }
        // The committed filter may have shrunk the row list; clamp
        // selection so it doesn't end up past the last visible row.
        self.clamp_selection_to_filter();
    }

    /// Push a character into the filter buffer at the cursor.
    pub fn filter_insert(&mut self, c: char) {
        if let Some(f) = self.filter.as_mut() {
            let byte = f
                .buffer
                .char_indices()
                .nth(f.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| f.buffer.len());
            f.buffer.insert(byte, c);
            f.cursor += 1;
        }
        self.clamp_selection_to_filter();
    }

    /// Delete the character before the cursor (Backspace).
    pub fn filter_backspace(&mut self) {
        if let Some(f) = self.filter.as_mut()
            && f.cursor > 0
        {
            let start = f
                .buffer
                .char_indices()
                .nth(f.cursor - 1)
                .map(|(b, _)| b)
                .unwrap_or(0);
            let end = f
                .buffer
                .char_indices()
                .nth(f.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| f.buffer.len());
            f.buffer.replace_range(start..end, "");
            f.cursor -= 1;
        }
        self.clamp_selection_to_filter();
    }

    /// Return the indices of `tab.issues` that pass the current
    /// filter, or `0..len` when there's none. Used by both the UI
    /// (to know what to render) and the keys layer (to translate
    /// selection navigation into raw `issues[]` indices).
    pub fn visible_indices(&self) -> Vec<usize> {
        let tab = self.active();
        let Some(filter) = self.filter.as_ref() else {
            return (0..tab.issues.len()).collect();
        };
        let needle = filter.buffer.to_ascii_lowercase();
        if needle.is_empty() {
            return (0..tab.issues.len()).collect();
        }
        tab.issues
            .iter()
            .enumerate()
            .filter_map(|(i, issue)| {
                let key_match = issue.key.to_ascii_lowercase().contains(&needle);
                let summary_match = issue.fields.summary.to_ascii_lowercase().contains(&needle);
                (key_match || summary_match).then_some(i)
            })
            .collect()
    }

    /// Clamp the active tab's `selected` index into the current
    /// filtered set. If the previously-selected row is filtered out,
    /// jumps to the first visible row.
    fn clamp_selection_to_filter(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let cur = self.active().selected;
        if !visible.contains(&cur) {
            self.active_mut().selected = visible[0];
        }
    }
}

/// Project key for an issue key like `TE-1234` ⇒ `TE`.
fn project_of(issue_key: &str) -> Option<String> {
    let mut parts = issue_key.splitn(2, '-');
    let project = parts.next()?;
    if project.is_empty() {
        None
    } else {
        Some(project.to_string())
    }
}

/// 2026-08-07 — split a JQL string into its `WHERE`-shaped prefix
/// and its trailing `ORDER BY …` clause (empty when absent). Used
/// to wrap the WHERE half in parens when adding an AND clause,
/// since Jira rejects `(<where> ORDER BY <keys>) AND <extra>` with
/// "Expecting ')' but got 'ORDER'". Case-insensitive; splits on the
/// LAST occurrence of `ORDER BY` outside a quoted string.
pub fn split_order_by(jql: &str) -> (String, String) {
    // Walk from the end, respecting quoted strings, looking for a
    // case-insensitive `ORDER BY`. Cheap: JQL is short, and this
    // runs at most once per pane refresh.
    let bytes = jql.as_bytes();
    let mut in_quote = false;
    let mut split_at: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            in_quote = !in_quote;
        } else if !in_quote
            && i + 8 <= bytes.len()
            && jql[i..i + 8].eq_ignore_ascii_case("order by")
        {
            split_at = Some(i);
        }
        i += 1;
    }
    match split_at {
        Some(idx) => (jql[..idx].trim().to_string(), jql[idx..].trim().to_string()),
        None => (jql.trim().to_string(), String::new()),
    }
}

/// 2026-08-07 — extract the value of a select-type Jira custom
/// field. The field id is caller-supplied (e.g. `customfield_10056`
/// for a "Team" select). Jira ships select-field values as
/// `{"self": "...", "value": "HeliOS", "id": "10725"}` — we only
/// care about `.value`. Returns None when the field isn't present
/// or isn't a select (missing `.value`).
pub fn team_value_of(issue: &crate::jira::Issue, field_id: &str) -> Option<String> {
    let raw = issue.fields.extras.get(field_id)?;
    raw.get("value")?.as_str().map(|s| s.to_string())
}

/// How `close_filter` should treat the in-progress buffer.
#[derive(Debug, Clone, Copy)]
pub enum FilterClose {
    /// Enter — keep what's typed (or drop to None if empty).
    Commit,
    /// Esc — discard whatever's typed and drop the filter entirely.
    Cancel,
}

impl App {
    /// Open the `t` transition picker for the focused ticket. Fires
    /// a transitions fetch; the picker renders a spinner state until
    /// it arrives. No-op when there's no focused ticket.
    pub async fn open_transition_picker(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        self.transition_picker = Some(TransitionPicker {
            key: key.clone(),
            transitions: None,
            selected: 0,
            error: None,
        });
        match self.client.fetch_transitions(&key).await {
            Ok(list) => {
                if let Some(p) = self.transition_picker.as_mut() {
                    p.transitions = Some(list);
                }
            }
            Err(e) => {
                if let Some(p) = self.transition_picker.as_mut() {
                    p.error = Some(e.to_string());
                    p.transitions = Some(Vec::new());
                }
            }
        }
    }

    /// Close the picker without firing a transition.
    pub fn close_transition_picker(&mut self) {
        self.transition_picker = None;
    }

    /// Move the picker highlight by `delta` rows, clamped to the
    /// loaded transitions list.
    pub fn transition_picker_move(&mut self, delta: isize) {
        if let Some(p) = self.transition_picker.as_mut()
            && let Some(list) = p.transitions.as_ref()
            && !list.is_empty()
        {
            let s = p.selected as isize + delta;
            p.selected = s.clamp(0, list.len() as isize - 1) as usize;
        }
    }

    /// Jump the picker highlight to row `idx` (used for digit keys
    /// 1-9). No-op if idx is out of range.
    pub fn transition_picker_select(&mut self, idx: usize) {
        if let Some(p) = self.transition_picker.as_mut()
            && let Some(list) = p.transitions.as_ref()
            && idx < list.len()
        {
            p.selected = idx;
        }
    }

    /// Open the assignee picker against the focused issue's project.
    /// Pre-fetches the assignable user list — empty query returns the
    /// first page; in-modal typing can re-query for longer lists.
    pub async fn open_assignee_picker(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        let Some(project) = project_of(&key) else {
            self.status = format!("can't derive project from {key}");
            return;
        };
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::Assignee,
            items: Vec::new(),
            loaded: false,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
        match self.client.fetch_assignable_users(&project, "").await {
            Ok(users) => {
                let items: Vec<(String, String)> = std::iter::once((
                    String::new(), // sentinel for "unassign"
                    "— Unassign —".to_string(),
                ))
                .chain(
                    users
                        .into_iter()
                        .filter(|u| !u.account_id.is_empty())
                        .map(|u| (u.account_id, u.display_name)),
                )
                .collect();
                if let Some(p) = self.field_picker.as_mut() {
                    p.items = items;
                    p.loaded = true;
                }
            }
            Err(e) => {
                if let Some(p) = self.field_picker.as_mut() {
                    p.error = Some(e.to_string());
                    p.loaded = true;
                }
            }
        }
    }

    /// Open the fixVersion picker. v1 sets a single version (overwrites
    /// whatever was there) — multi-version editing can come later.
    pub async fn open_fix_version_picker(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        let Some(project) = project_of(&key) else {
            self.status = format!("can't derive project from {key}");
            return;
        };
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::FixVersion,
            items: Vec::new(),
            loaded: false,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
        match self.client.fetch_versions(&project).await {
            Ok(versions) => {
                let items: Vec<(String, String)> =
                    std::iter::once((String::new(), "— Clear fixVersion —".to_string()))
                        .chain(versions.into_iter().map(|v| {
                            let label = if v.released {
                                format!("{} (released)", v.name)
                            } else {
                                v.name.clone()
                            };
                            (v.name, label)
                        }))
                        .collect();
                if let Some(p) = self.field_picker.as_mut() {
                    p.items = items;
                    p.loaded = true;
                }
            }
            Err(e) => {
                if let Some(p) = self.field_picker.as_mut() {
                    p.error = Some(e.to_string());
                    p.loaded = true;
                }
            }
        }
    }

    pub fn close_field_picker(&mut self) {
        self.field_picker = None;
    }

    /// 2026-08-06 — Team picker. Collects unique component names +
    /// label strings from every issue in the active tab, plus a
    /// leading "— Clear team —" row. Selecting commits the value
    /// into `cfg.tabs[active].team` (in-memory only — restart reloads
    /// from config.toml) and re-buckets the kanban on next paint.
    pub fn open_team_picker(&mut self) {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for issue in &self.active().issues {
            for c in &issue.fields.components {
                if !c.name.trim().is_empty() {
                    seen.insert(c.name.clone());
                }
            }
            for l in &issue.fields.labels {
                if !l.trim().is_empty() {
                    seen.insert(l.clone());
                }
            }
            // Include the configured team-select custom-field value
            // (e.g. team names from a "Team" select field
            // at customfield_10056 — user-configured via
            // `team_field_id`). Silently skipped when no field is
            // configured or the issue doesn't carry the field.
            if let Some(id) = self
                .cfg
                .team_field_id
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                && let Some(v) = team_value_of(issue, id)
                && !v.trim().is_empty()
            {
                seen.insert(v);
            }
        }
        let items: Vec<(String, String)> =
            std::iter::once((String::new(), "— Clear team —".to_string()))
                .chain(seen.into_iter().map(|s| (s.clone(), s)))
                .collect();
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::Team,
            items,
            loaded: true,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
    }

    /// 2026-08-07 — Actions picker (`.` key). Lists whichever
    /// action buttons `dispatch::buttons_for_ticket` recommends for
    /// the focused ticket (Implement / Fix / Triage). No items ⇒
    /// toast a hint instead of opening an empty picker.
    pub fn open_action_picker(&mut self) {
        let Some(issue) = self.active().issues.get(self.active().selected) else {
            return;
        };
        let buttons = crate::dispatch::buttons_for_ticket(issue);
        if buttons.is_empty() {
            self.status = format!(
                ". actions: no ticket-level actions for {} ({} · {})",
                issue.key,
                issue
                    .fields
                    .issuetype
                    .as_ref()
                    .map(|t| t.name.as_str())
                    .unwrap_or("?"),
                issue
                    .fields
                    .status
                    .as_ref()
                    .map(|s| s.name.as_str())
                    .unwrap_or("?"),
            );
            return;
        }
        let items: Vec<(String, String)> = buttons
            .iter()
            .map(|b| (b.kind().to_string(), b.label().to_string()))
            .collect();
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::TicketAction,
            items,
            loaded: true,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
    }

    /// #1004 (2026-08-18) — Issue Type picker. Items = the union of
    /// `issuetype.name` across the tab's issues. Prepended with a
    /// "— Clear type —" row that commits `None`.
    pub fn open_issue_type_picker(&mut self) {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for issue in &self.active().issues {
            if let Some(t) = &issue.fields.issuetype
                && !t.name.trim().is_empty()
            {
                seen.insert(t.name.clone());
            }
        }
        let items: Vec<(String, String)> =
            std::iter::once((String::new(), "— Clear type —".to_string()))
                .chain(seen.into_iter().map(|s| (s.clone(), s)))
                .collect();
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::IssueType,
            items,
            loaded: true,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
    }

    /// #1004 (2026-08-18) — Commit for the Issue Type picker.
    pub fn commit_issue_type_picker(&mut self, value: String) {
        let idx = self.active_tab;
        if let Some(tab) = self.cfg.tabs.get_mut(idx) {
            tab.issue_type = if value.trim().is_empty() {
                None
            } else {
                Some(value)
            };
        }
        self.field_picker = None;
    }

    /// #1004 (2026-08-18) — Label picker. Items = the union of label
    /// strings across the tab's issues, prepended with a "— Clear
    /// label —" row.
    pub fn open_label_picker(&mut self) {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for issue in &self.active().issues {
            for l in &issue.fields.labels {
                if !l.trim().is_empty() {
                    seen.insert(l.clone());
                }
            }
        }
        let items: Vec<(String, String)> =
            std::iter::once((String::new(), "— Clear label —".to_string()))
                .chain(seen.into_iter().map(|s| (s.clone(), s)))
                .collect();
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::Label,
            items,
            loaded: true,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
    }

    /// #1004 (2026-08-18) — Commit for the Label picker.
    pub fn commit_label_picker(&mut self, value: String) {
        let idx = self.active_tab;
        if let Some(tab) = self.cfg.tabs.get_mut(idx) {
            tab.label = if value.trim().is_empty() {
                None
            } else {
                Some(value)
            };
        }
        self.field_picker = None;
    }

    /// Commit handler for the Team picker. Writes the value onto the
    /// active tab config; the kanban render reads from there on next
    /// paint. `""` clears the filter.
    pub fn commit_team_picker(&mut self, value: String) {
        let idx = self.active_tab;
        if let Some(tab) = self.cfg.tabs.get_mut(idx) {
            tab.team = if value.trim().is_empty() {
                None
            } else {
                Some(value)
            };
        }
        self.field_picker = None;
    }

    /// 2026-08-17 (task #887) — Sprint picker for board tabs. Opens
    /// a modal listing the tab's board sprints (current + upcoming +
    /// last N closed). Uses the cache when populated, otherwise
    /// fetches from `/rest/agile/1.0/board/{id}/sprint`. Silently
    /// no-ops for tabs without a `board_id`.
    ///
    /// Kanban boards return an empty sprint list (the API rejects
    /// the `/sprint` endpoint with 400 for non-scrum boards, which
    /// the client maps to `Ok(vec![])`); the chip that fires this
    /// picker is hidden on those tabs, so getting here already
    /// implies scrum.
    pub async fn open_sprint_picker(&mut self) {
        let idx = self.active_tab;
        let Some(board_id) = self.cfg.tabs.get(idx).and_then(|t| t.board_id) else {
            self.status = "sprint picker: this tab has no `board_id`".to_string();
            return;
        };
        // Prime the picker in the "loading…" state before we await —
        // gives the UI a chance to render feedback if the fetch is
        // slow.
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::Sprint,
            items: Vec::new(),
            loaded: false,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
        let sprints = if let Some(cached) = self.tabs[idx].sprints_cache.clone() {
            cached
        } else {
            match self.client.fetch_sprints_for_board(board_id).await {
                Ok(list) => {
                    self.tabs[idx].sprints_cache = Some(list.clone());
                    list
                }
                Err(e) => {
                    if let Some(p) = self.field_picker.as_mut() {
                        p.error = Some(e.to_string());
                        p.loaded = true;
                    }
                    return;
                }
            }
        };
        // Cap `last N closed` at 5 — the picker window is ~13 rows
        // tall; leaving room for active + 2-3 future keeps closed
        // sprints from pushing the current sprint out of view.
        let sorted = Sprint::sort_for_picker(sprints, 5);
        // The "— Board default (active sprint) —" sentinel maps to
        // `selected_sprint_id = None` so users can quickly return to
        // the board's default view (which follows the active sprint
        // rotation without a re-pick).
        let items: Vec<(String, String)> = std::iter::once((
            String::new(),
            "— Board default (active sprint) —".to_string(),
        ))
        .chain(sorted.iter().map(|s| {
            let tag = match s.state.to_ascii_lowercase().as_str() {
                "active" => "active",
                "future" => "future",
                _ => "closed",
            };
            (s.id.to_string(), format!("{}  [{tag}]", s.name))
        }))
        .collect();
        if items.len() == 1 {
            // Sentinel only ⇒ no sprints (this board is kanban after
            // all, or scrum with none created yet). Nicer to close +
            // toast than to open an empty picker.
            self.field_picker = None;
            self.status = "sprint picker: this board has no sprints".to_string();
            return;
        }
        // Pre-select whatever the tab is currently pinned to, so the
        // picker opens with the cursor on the user's current view.
        let current = self.tabs[idx]
            .selected_sprint_id
            .map(|id| id.to_string())
            .unwrap_or_default();
        let default_pos = items.iter().position(|(id, _)| id == &current).unwrap_or(0);
        // The picker's `selected` is an INDEX into `items` (visible_indices
        // returns [0..items.len) when the filter is empty). Set it to
        // the current-sprint row.
        let picker_selected = default_pos;
        if let Some(p) = self.field_picker.as_mut() {
            p.items = items;
            p.selected = picker_selected;
            p.loaded = true;
        }
    }

    /// 2026-08-17 (task #887) — commit for the Sprint picker. `id`
    /// is either an empty string (Board default) or the sprint id
    /// as a decimal string. Updates the tab's `selected_sprint_id`
    /// and refetches so the kanban re-populates from the chosen
    /// sprint.
    pub async fn commit_sprint_picker(&mut self, id: String) {
        let idx = self.active_tab;
        let new_id: Option<u64> = if id.trim().is_empty() {
            None
        } else {
            id.parse::<u64>().ok()
        };
        self.tabs[idx].selected_sprint_id = new_id;
        // Reset the kanban column scroll — the new sprint's tickets
        // are unrelated to the old sprint's rows.
        self.kanban_col_scroll = [0; 4];
        self.field_picker = None;
        self.status = match new_id {
            Some(id) => format!("sprint: pinned to {id}"),
            None => "sprint: back to board default (active)".to_string(),
        };
        self.refresh_active().await;
    }

    /// 2026-08-17 (task #893) — Quick-filter picker for board tabs.
    /// Multi-select. Opens with the currently-active filters ticked;
    /// Space toggles a row; Enter closes + refetches.
    pub async fn open_quickfilter_picker(&mut self) {
        let idx = self.active_tab;
        let Some(board_id) = self.cfg.tabs.get(idx).and_then(|t| t.board_id) else {
            self.status = "quick filters: this tab has no `board_id`".to_string();
            return;
        };
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::QuickFilter,
            items: Vec::new(),
            loaded: false,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: Some(BTreeSet::new()),
        });
        let filters = if let Some(cached) = self.tabs[idx].quick_filters_cache.clone() {
            cached
        } else {
            match self.client.fetch_quickfilters_for_board(board_id).await {
                Ok(list) => {
                    self.tabs[idx].quick_filters_cache = Some(list.clone());
                    list
                }
                Err(e) => {
                    if let Some(p) = self.field_picker.as_mut() {
                        p.error = Some(e.to_string());
                        p.loaded = true;
                    }
                    return;
                }
            }
        };
        if filters.is_empty() {
            // Nothing to pick ⇒ close + toast instead of an empty modal.
            self.field_picker = None;
            self.status = "quick filters: this board defines none".to_string();
            return;
        }
        // Seed multi-select with whatever is already active so re-open
        // shows the current state, not a blank slate.
        let mut multi = BTreeSet::new();
        for qf_id in &self.tabs[idx].active_quick_filter_ids {
            multi.insert(qf_id.to_string());
        }
        let items: Vec<(String, String)> = filters
            .iter()
            .map(|qf| (qf.id.to_string(), qf.name.clone()))
            .collect();
        if let Some(p) = self.field_picker.as_mut() {
            p.items = items;
            p.multi_selected = Some(multi);
            p.loaded = true;
        }
    }

    /// 2026-08-17 (task #893) — Space in the quick-filter picker:
    /// toggle the row under the cursor in `multi_selected`. No-op
    /// for pickers without multi-select on.
    pub fn quickfilter_toggle_selected(&mut self) {
        let Some(p) = self.field_picker.as_mut() else {
            return;
        };
        let Some(multi) = p.multi_selected.as_mut() else {
            return;
        };
        let Some((id, _)) = p.items.get(p.selected).cloned() else {
            return;
        };
        if !multi.remove(&id) {
            multi.insert(id);
        }
    }

    /// 2026-08-17 (task #893) — commit for the Quick-filter picker.
    /// Writes `multi_selected` back into the tab's
    /// `active_quick_filter_ids` set and refetches.
    pub async fn commit_quickfilter_picker(&mut self) {
        let idx = self.active_tab;
        let Some(p) = self.field_picker.as_ref() else {
            return;
        };
        let Some(multi) = p.multi_selected.as_ref() else {
            return;
        };
        let new_ids: BTreeSet<u64> = multi.iter().filter_map(|s| s.parse::<u64>().ok()).collect();
        self.tabs[idx].active_quick_filter_ids = new_ids;
        self.field_picker = None;
        let n = self.tabs[idx].active_quick_filter_ids.len();
        self.status = if n == 0 {
            "quick filters: cleared".to_string()
        } else {
            format!("quick filters: {n} active")
        };
        self.refresh_active().await;
    }

    /// 2026-08-17 (task #893) — open the current board's settings
    /// page in the system browser. Jira Cloud's canonical URL is
    /// `${jira_url}/jira/software/c/projects/<PROJECT>/boards/<ID>?config=filter`,
    /// which jumps straight into the configuration tab.
    /// Server-hosted / older instances also honor the classic
    /// `${jira_url}/secure/RapidBoard.jspa?rapidView=<ID>&config=filter`
    /// URL — we ship the Cloud form because Atlassian Cloud is the
    /// baseline mnml-tracker-jira targets, and Cloud redirects
    /// unknown project paths back to the general boards list.
    pub fn open_board_settings(&mut self) {
        let idx = self.active_tab;
        let Some(tab_cfg) = self.cfg.tabs.get(idx) else {
            return;
        };
        let Some(board_id) = tab_cfg.board_id else {
            self.status = "board settings: this tab has no `board_id`".to_string();
            return;
        };
        let base = self.client.base_url();
        let url = match tab_cfg.project.as_ref() {
            Some(project) => {
                format!("{base}/jira/software/c/projects/{project}/boards/{board_id}?config=filter")
            }
            // No project on the tab (unlikely for board_active_sprint
            // / board_backlog — validate() requires it — but not
            // impossible for a user-authored `kind=None` tab with
            // `board_id`). Fall back to the classic RapidBoard URL
            // which doesn't need a project.
            None => format!("{base}/secure/RapidBoard.jspa?rapidView={board_id}&config=filter"),
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = "opened board settings in browser".to_string(),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// 2026-08-06 — Fix Versions tab-view picker. Opens on `V`. Same
    /// item source as the ticket-field picker (`f`), but committing
    /// rewrites the tab's JQL to filter the whole tab to that version
    /// (vs. `f` which POSTs the assignment to Jira for the focused
    /// ticket). Only meaningful on fix_version_tree tabs.
    pub async fn open_tab_fix_version_picker(&mut self) {
        let Some(tab_cfg) = self.cfg.tabs.get(self.active_tab) else {
            return;
        };
        let Some(project) = tab_cfg.project.clone() else {
            self.status = "V: tab has no `project`".to_string();
            return;
        };
        self.field_picker = Some(FieldPicker {
            kind: FieldKind::TabFixVersion,
            items: Vec::new(),
            loaded: false,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
            multi_selected: None,
        });
        match self.client.fetch_versions(&project).await {
            Ok(versions) => {
                let items: Vec<(String, String)> = versions
                    .into_iter()
                    .map(|v| {
                        let label = if v.released {
                            format!("{} (released)", v.name)
                        } else {
                            v.name.clone()
                        };
                        (v.name, label)
                    })
                    .collect();
                if let Some(p) = self.field_picker.as_mut() {
                    p.items = items;
                    p.loaded = true;
                }
            }
            Err(e) => {
                if let Some(p) = self.field_picker.as_mut() {
                    p.error = Some(e.to_string());
                    p.loaded = true;
                }
            }
        }
    }

    /// Commit for the TabFixVersion picker — rewrite the current
    /// tab's jql to filter on this version + refresh.
    pub async fn commit_tab_fix_version_picker(&mut self, version: String) {
        let idx = self.active_tab;
        if let Some(tab) = self.cfg.tabs.get_mut(idx) {
            let Some(project) = tab.project.clone() else {
                self.status = "V: tab has no `project`".to_string();
                self.field_picker = None;
                return;
            };
            // Escape any `"` in the version name (Jira allows them though
            // rare). JQL uses `\"` inside a double-quoted string.
            let escaped = version.replace('"', "\\\"");
            let jql = format!("project = {project} AND fixVersion = \"{escaped}\" ORDER BY rank");
            tab.jql = Some(jql.clone());
            // Also mirror to the runtime TabState.jql so the resolver
            // sees it without a full reload.
            if let Some(state) = self.tabs.get_mut(idx) {
                state.jql = jql;
            }
        }
        self.field_picker = None;
        self.status = format!("tab view: fixVersion = {version}");
        self.refresh_active().await;
    }

    pub fn field_picker_filter_insert(&mut self, c: char) {
        if let Some(p) = self.field_picker.as_mut() {
            let byte = p
                .filter
                .char_indices()
                .nth(p.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| p.filter.len());
            p.filter.insert(byte, c);
            p.cursor += 1;
            // Keep highlight inside the filtered set.
            let visible = p.visible_indices();
            if !visible.is_empty() && !visible.contains(&p.selected) {
                p.selected = visible[0];
            }
        }
    }

    pub fn field_picker_filter_backspace(&mut self) {
        if let Some(p) = self.field_picker.as_mut()
            && p.cursor > 0
        {
            let start = p
                .filter
                .char_indices()
                .nth(p.cursor - 1)
                .map(|(b, _)| b)
                .unwrap_or(0);
            let end = p
                .filter
                .char_indices()
                .nth(p.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| p.filter.len());
            p.filter.replace_range(start..end, "");
            p.cursor -= 1;
        }
    }

    pub fn field_picker_move(&mut self, delta: isize) {
        if let Some(p) = self.field_picker.as_mut() {
            let visible = p.visible_indices();
            if visible.is_empty() {
                return;
            }
            let pos = visible.iter().position(|&i| i == p.selected).unwrap_or(0) as isize;
            let new_pos = (pos + delta).clamp(0, visible.len() as isize - 1) as usize;
            p.selected = visible[new_pos];
        }
    }

    /// Commit the field picker against the focused ticket (or the
    /// whole selection if non-empty). Same bulk-aware shape as
    /// `commit_transition`.
    pub async fn commit_field_picker(&mut self) {
        let Some(picker) = self.field_picker.as_ref() else {
            return;
        };
        if !picker.loaded {
            return;
        }
        let Some((id, label)) = picker.items.get(picker.selected).cloned() else {
            return;
        };
        let kind = picker.kind;
        // Team picker is a local filter, not a Jira mutation. Route
        // before the bulk-write loop below so it doesn't try to POST
        // anything to Jira.
        if kind == FieldKind::Team {
            self.commit_team_picker(id.clone());
            let display = if id.is_empty() {
                "(cleared)".to_string()
            } else {
                label.clone()
            };
            self.status = format!("team filter: {display}");
            // 2026-08-07 — server-side team filter needs a fresh
            // fetch. Was: only re-bucketed the already-fetched 100
            // tickets client-side, which meant HeliOS's tickets past
            // the openSprints() rank cap were missing.
            self.refresh_active().await;
            return;
        }
        // #1004 (2026-08-18) — Type + Label are client-side render
        // filters. No refetch needed; just re-bucket on next paint.
        if kind == FieldKind::IssueType {
            self.commit_issue_type_picker(id.clone());
            let display = if id.is_empty() {
                "(cleared)".to_string()
            } else {
                label.clone()
            };
            self.status = format!("type filter: {display}");
            return;
        }
        if kind == FieldKind::Label {
            self.commit_label_picker(id.clone());
            let display = if id.is_empty() {
                "(cleared)".to_string()
            } else {
                label.clone()
            };
            self.status = format!("label filter: {display}");
            return;
        }
        // TabFixVersion picker rewrites the tab's JQL — not a per-
        // ticket assignment. Route before the bulk-write loop.
        if kind == FieldKind::TabFixVersion {
            self.commit_tab_fix_version_picker(id.clone()).await;
            return;
        }
        // Action picker: fire the ticket-level dispatch (Implement /
        // Fix / Triage). Same handoff the on-card buttons use.
        if kind == FieldKind::TicketAction {
            self.field_picker = None;
            self.dispatch_ticket_action(Box::leak(id.clone().into_boxed_str()));
            return;
        }
        // Sprint picker (task #887): commit updates the tab's
        // `selected_sprint_id` and refetches. Route before the
        // per-ticket bulk-write loop.
        if kind == FieldKind::Sprint {
            self.commit_sprint_picker(id.clone()).await;
            return;
        }
        // Quick-filter picker (task #893): multi-select commit
        // updates the tab's `active_quick_filter_ids` set and
        // refetches. Route before the per-ticket bulk-write loop.
        if kind == FieldKind::QuickFilter {
            self.commit_quickfilter_picker().await;
            return;
        }
        let keys: Vec<String> = if self.selection.is_empty() {
            self.focused_key().into_iter().collect()
        } else {
            self.selection.iter().cloned().collect()
        };
        if keys.is_empty() {
            return;
        }
        let mut ok = 0usize;
        let mut errors: Vec<String> = Vec::new();
        for key in &keys {
            let result = match kind {
                FieldKind::Assignee => {
                    let account = if id.is_empty() {
                        None
                    } else {
                        Some(id.as_str())
                    };
                    self.client.set_assignee(key, account).await
                }
                FieldKind::FixVersion => {
                    let versions: Vec<String> = if id.is_empty() {
                        Vec::new()
                    } else {
                        vec![id.clone()]
                    };
                    self.client.set_fix_versions(key, &versions).await
                }
                FieldKind::Team
                | FieldKind::TabFixVersion
                | FieldKind::TicketAction
                | FieldKind::Sprint
                | FieldKind::QuickFilter
                | FieldKind::IssueType
                | FieldKind::Label => {
                    unreachable!("routed above")
                }
            };
            match result {
                Ok(()) => {
                    ok += 1;
                    self.detail_cache.remove(key);
                }
                Err(e) => errors.push(format!("{key}: {e}")),
            }
        }
        if errors.is_empty() {
            self.field_picker = None;
            let field = match kind {
                FieldKind::Assignee => "assignee",
                FieldKind::FixVersion => "fixVersion",
                FieldKind::Team
                | FieldKind::TabFixVersion
                | FieldKind::TicketAction
                | FieldKind::Sprint
                | FieldKind::QuickFilter
                | FieldKind::IssueType
                | FieldKind::Label => {
                    unreachable!("routed above")
                }
            };
            self.status = format!("{} ticket(s) · {field} = {label}", ok);
            self.selection.clear();
        } else if let Some(p) = self.field_picker.as_mut() {
            p.error = Some(format!(
                "{} ok · {} failed — {}",
                ok,
                errors.len(),
                errors.join(" / ")
            ));
        }
        self.refresh_active().await;
        if self.details_visible {
            self.ensure_focused_detail().await;
        }
    }

    /// Toggle the focused ticket's key in the selection set.
    pub fn toggle_selection(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        if !self.selection.remove(&key) {
            self.selection.insert(key);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// The set of keys a bulk operation should run against — the
    /// selection if non-empty, else just the focused row's key.
    /// Used by [`Self::commit_transition`] (via direct `self.selection`
    /// check) and the future bulk-assign/bulk-fixVersion paths.
    #[allow(dead_code)]
    pub fn bulk_keys(&self) -> Vec<String> {
        if !self.selection.is_empty() {
            self.selection.iter().cloned().collect()
        } else {
            self.focused_key().into_iter().collect()
        }
    }

    /// Open the inline comment editor for the focused ticket. No-op
    /// unless the detail panel is visible — without it there'd be
    /// nowhere to render the editor.
    pub fn open_comment_editor(&mut self) {
        if !self.details_visible {
            return;
        }
        let Some(key) = self.focused_key() else {
            return;
        };
        self.comment_editor = Some(CommentEditor {
            key,
            buffer: String::new(),
            cursor: 0,
            posting: false,
            error: None,
        });
    }

    pub fn close_comment_editor(&mut self) {
        self.comment_editor = None;
    }

    pub fn comment_editor_insert(&mut self, c: char) {
        if let Some(e) = self.comment_editor.as_mut()
            && !e.posting
        {
            let byte = e
                .buffer
                .char_indices()
                .nth(e.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| e.buffer.len());
            e.buffer.insert(byte, c);
            e.cursor += 1;
        }
    }

    pub fn comment_editor_backspace(&mut self) {
        if let Some(e) = self.comment_editor.as_mut()
            && !e.posting
            && e.cursor > 0
        {
            let start = e
                .buffer
                .char_indices()
                .nth(e.cursor - 1)
                .map(|(b, _)| b)
                .unwrap_or(0);
            let end = e
                .buffer
                .char_indices()
                .nth(e.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| e.buffer.len());
            e.buffer.replace_range(start..end, "");
            e.cursor -= 1;
        }
    }

    /// POST the comment to Jira. On success drops the editor, refreshes
    /// the cached detail (so the new comment appears in the thread), and
    /// toasts. On failure surfaces the error inside the editor + leaves
    /// it open so the user can retry or copy the text out.
    pub async fn submit_comment(&mut self) {
        let Some(editor) = self.comment_editor.as_ref() else {
            return;
        };
        if editor.buffer.trim().is_empty() || editor.posting {
            return;
        }
        let key = editor.key.clone();
        let body = editor.buffer.clone();
        if let Some(e) = self.comment_editor.as_mut() {
            e.posting = true;
            e.error = None;
        }
        match self.client.post_comment(&key, &body).await {
            Ok(()) => {
                self.comment_editor = None;
                self.status = format!("commented on {key}");
                self.detail_cache.remove(&key);
                if self.details_visible {
                    self.ensure_focused_detail().await;
                }
            }
            Err(e) => {
                if let Some(ed) = self.comment_editor.as_mut() {
                    ed.posting = false;
                    ed.error = Some(e.to_string());
                }
            }
        }
    }

    /// Toggle watch state on the focused ticket. Direction is
    /// derived from `detail.watching` — needs the detail cached
    /// (force-fetches if not), so the toggle reflects the current
    /// server state. After the API call succeeds we drop the cached
    /// detail for this key so the next render shows the updated
    /// watcher count.
    pub async fn toggle_watch(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        // Make sure the detail is loaded — we need `watching` to know
        // which direction to toggle.
        self.ensure_focused_detail().await;
        let was_watching = self
            .detail_cache
            .get(&key)
            .map(|d| d.watching)
            .unwrap_or(false);
        let result = if was_watching {
            // Unwatch needs the authenticated user's accountId.
            let account_id = match self.fetch_or_cached_account_id().await {
                Some(id) => id,
                None => {
                    return; // Status line already explains.
                }
            };
            self.client.unwatch_issue(&key, &account_id).await
        } else {
            self.client.watch_issue(&key).await
        };
        match result {
            Ok(()) => {
                let verb = if was_watching { "unwatched" } else { "watched" };
                self.status = format!("{verb} {key}");
                // The watch_count + isWatching on the server changed;
                // drop the cache so re-render shows fresh state.
                self.detail_cache.remove(&key);
                if self.details_visible {
                    self.ensure_focused_detail().await;
                }
            }
            Err(e) => {
                self.status = format!("watch toggle failed for {key}: {e}");
            }
        }
    }

    /// Lazy-fetch the authenticated user's accountId, caching the
    /// success / permanent-failure result on `self.my_account_id`.
    /// Returns `None` and toasts the error on failure.
    async fn fetch_or_cached_account_id(&mut self) -> Option<String> {
        if let Some(slot) = self.my_account_id.as_ref() {
            return match slot {
                Ok(id) => Some(id.clone()),
                Err(e) => {
                    self.status = format!("can't unwatch — myself fetch failed earlier: {e}");
                    None
                }
            };
        }
        match self.client.myself().await {
            Ok(id) => {
                self.my_account_id = Some(Ok(id.clone()));
                Some(id)
            }
            Err(e) => {
                let msg = e.to_string();
                self.my_account_id = Some(Err(msg.clone()));
                self.status = format!("myself fetch failed: {msg}");
                None
            }
        }
    }

    /// Commit the highlighted transition. When `selection` is empty
    /// this is the single-ticket case. When selection is non-empty,
    /// the chosen transition's **name** ("Start review") is fired
    /// against every selected key — each may have a different id for
    /// the same name (Jira workflows aren't required to use stable
    /// ids across projects), so we fetch the per-issue transitions
    /// list and pick by name match. Issues that don't have a matching
    /// transition are skipped (terminal state / different workflow);
    /// per-issue errors aggregate into the picker's error slot.
    pub async fn commit_transition(&mut self) {
        let Some(p) = self.transition_picker.as_ref() else {
            return;
        };
        let Some(list) = p.transitions.as_ref() else {
            return;
        };
        let Some(transition) = list.get(p.selected) else {
            return;
        };
        let transition_name = transition.name.clone();
        let to_name = transition
            .to_name
            .clone()
            .unwrap_or_else(|| transition.name.clone());

        // Single-ticket case — fire by id, fastest path. Triggered
        // when selection is empty.
        if self.selection.is_empty() {
            let key = p.key.clone();
            let transition_id = transition.id.clone();
            match self.client.run_transition(&key, &transition_id).await {
                Ok(()) => {
                    self.transition_picker = None;
                    self.status = format!("{key} → {to_name}");
                    self.detail_cache.remove(&key);
                    self.refresh_active().await;
                    if self.details_visible {
                        self.ensure_focused_detail().await;
                    }
                }
                Err(e) => {
                    if let Some(p) = self.transition_picker.as_mut() {
                        p.error = Some(e.to_string());
                    }
                }
            }
            return;
        }

        // Bulk case — N selected keys, one transition name. Per-issue:
        //   1. fetch transitions
        //   2. find by name
        //   3. fire if found, else record "no matching transition"
        let keys: Vec<String> = self.selection.iter().cloned().collect();
        let mut ok = 0usize;
        let mut skipped: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for key in &keys {
            let id = match self.client.fetch_transitions(key).await {
                Ok(list) => list
                    .into_iter()
                    .find(|t| t.name.eq_ignore_ascii_case(&transition_name))
                    .map(|t| t.id),
                Err(e) => {
                    errors.push(format!("{key}: {e}"));
                    continue;
                }
            };
            let Some(id) = id else {
                skipped.push(key.clone());
                continue;
            };
            match self.client.run_transition(key, &id).await {
                Ok(()) => {
                    ok += 1;
                    self.detail_cache.remove(key);
                }
                Err(e) => errors.push(format!("{key}: {e}")),
            }
        }

        if errors.is_empty() {
            // Clean run — clear selection + picker.
            self.transition_picker = None;
            self.selection.clear();
            let mut msg = format!("{} ticket(s) → {to_name}", ok);
            if !skipped.is_empty() {
                msg.push_str(&format!(
                    " · skipped {}: {}",
                    skipped.len(),
                    skipped.join(", ")
                ));
            }
            self.status = msg;
        } else {
            // Surface aggregate failures in the picker so the user sees
            // them; keep selection intact for retry.
            if let Some(p) = self.transition_picker.as_mut() {
                p.error = Some(format!(
                    "{} ok · {} skipped · {} failed — {}",
                    ok,
                    skipped.len(),
                    errors.len(),
                    errors.join(" / ")
                ));
            }
        }
        self.refresh_active().await;
        if self.details_visible {
            self.ensure_focused_detail().await;
        }
    }

    /// 2026-08-07 — fetch a board's friendly name and cache it.
    /// No-op when already cached. Called from the kanban render
    /// path (fire-and-forget via `.await` inside the event loop's
    /// pre-draw hook — see `ensure_board_names_for_active`).
    pub async fn resolve_board_name(&mut self, board_id: u64) {
        if self.board_name_cache.contains_key(&board_id) {
            return;
        }
        match self.client.fetch_board(board_id).await {
            Ok(b) => {
                self.board_name_cache.insert(board_id, b.name);
            }
            Err(e) => {
                self.status = format!("board {board_id} lookup: {e}");
                // Cache a fallback so we don't hammer the endpoint
                // every frame when the id is bad.
                self.board_name_cache
                    .insert(board_id, format!("{board_id}"));
            }
        }
    }

    /// Called from the event loop before each draw of a kanban
    /// tab. Ensures the friendly name for the current tab's
    /// `board_id` (if any) is cached — the render then reads from
    /// `board_name_cache`.
    pub async fn ensure_board_names_for_active(&mut self) {
        let Some(bid) = self.cfg.tabs.get(self.active_tab).and_then(|t| t.board_id) else {
            return;
        };
        self.resolve_board_name(bid).await;
    }

    /// 2026-08-07 — open the ticket detail modal for `key` and
    /// kick off the fetch. Fields = whatever `[detail_modal]` in
    /// the user's config asks for (plus a couple always-on ones
    /// like summary/status so the header renders even for a
    /// minimal config).
    pub async fn open_detail_modal(&mut self, key: String) {
        let alias = self.cfg.detail_modal.field_alias.clone();
        let mut fields: Vec<String> = self
            .cfg
            .detail_modal
            .fields
            .iter()
            .map(|f| f.resolve_id(&alias))
            .collect();
        // Always include the ones the header + fallbacks need.
        for baked in [
            "summary",
            "status",
            "issuetype",
            "priority",
            "assignee",
            "reporter",
            "labels",
            "components",
            "fixVersions",
            "parent",
            "description",
        ] {
            let s = baked.to_string();
            if !fields.contains(&s) {
                fields.push(s);
            }
        }
        self.detail_modal = Some(DetailModal {
            key: key.clone(),
            data: None,
            scroll: 0,
            error: None,
        });
        match self.client.fetch_issue_full(&key, &fields).await {
            Ok(v) => {
                if let Some(m) = self.detail_modal.as_mut()
                    && m.key == key
                {
                    m.data = Some(v);
                }
            }
            Err(e) => {
                if let Some(m) = self.detail_modal.as_mut()
                    && m.key == key
                {
                    m.error = Some(format!("{e}"));
                }
            }
        }
    }

    pub fn close_detail_modal(&mut self) {
        self.detail_modal = None;
    }

    pub fn detail_modal_scroll(&mut self, delta: i32) {
        if let Some(m) = self.detail_modal.as_mut() {
            let cur = m.scroll as i32;
            m.scroll = (cur + delta).max(0) as u16;
        }
    }

    /// Toggle inline expand-chevron state for one card.
    pub fn toggle_kanban_expanded(&mut self, key: &str) {
        if self.kanban_expanded.contains(key) {
            self.kanban_expanded.remove(key);
        } else {
            self.kanban_expanded.insert(key.to_string());
        }
    }

    /// True when the active tab is a kanban (board) view.
    pub fn active_is_kanban(&self) -> bool {
        self.cfg
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.kind)
            .is_some_and(|k| matches!(k, TabKind::BoardActiveSprint | TabKind::BoardBacklog))
    }

    /// Scroll one column on the active kanban tab. `col` is 0..4
    /// (To Do / In Progress / Testing / Done order).
    pub fn kanban_scroll_col(&mut self, col: usize, delta: i32) {
        if col >= 4 {
            return;
        }
        let cur = self.kanban_col_scroll[col] as i32;
        self.kanban_col_scroll[col] = (cur + delta).max(0) as u16;
    }

    /// The kanban column that currently owns the cursor (0..4).
    pub fn kanban_selected_col(&self) -> usize {
        let tab = self.active();
        let Some(issue) = tab.issues.get(tab.selected) else {
            return 1;
        };
        let s = issue
            .fields
            .status
            .as_ref()
            .map(|s| s.name.to_ascii_lowercase())
            .unwrap_or_default();
        match s.as_str() {
            "to do" | "backlog" | "open" | "reopened" | "selected for development" => 0,
            "done" | "closed" | "resolved" | "released" => 3,
            "testing" | "in pr review" | "in review" | "qa" | "ready for qa" | "code review" => 2,
            _ => 1,
        }
    }

    /// Reveal ALL remaining linked PRs for `key` on the active tab.
    /// 2026-08-18 (#994) — was `cur + 3` which produced a staircase
    /// ("show 10 more" → 6 visible → "show 7 more" → 9 visible → ...
    /// taking 4+ clicks to see everything). Now one click reveals
    /// everything. Setting to usize::MAX is fine — the render clamps
    /// to the actual issue.linked_prs.len().
    pub fn pr_show_more(&mut self, key: &str) {
        let Some(tree) = self.active_mut().tree.as_mut() else {
            return;
        };
        tree.pr_show_counts.insert(key.to_string(), usize::MAX);
    }
}

#[cfg(test)]
impl App {
    /// Build a minimal App for unit tests in sibling modules (e.g.
    /// keys.rs). Single tab with no kind; sync (no jira client init).
    /// Sibling tests tweak cfg.tabs[0].kind or state as needed.
    pub(crate) fn test_app_empty() -> Self {
        let client = Client::new("https://example.atlassian.net", "x@y.z", "tok").unwrap();
        Self {
            cfg: Config {
                jira_url: "https://example.atlassian.net".to_string(),
                email: "x@y.z".to_string(),
                refresh_interval_secs: 60,
                tabs: vec![Tab {
                    name: "Test".to_string(),
                    kind: None,
                    jql: Some("project = TE".to_string()),
                    mode: None,
                    project: Some("TE".to_string()),
                    component: None,
                    version_name_contains: None,
                    board_id: None,
                    team: None,
                    columns: None,
                    bumps: Default::default(),
                    status_order: None,
                }],
                release_cut: false,
                team_field_id: None,
                team_field_name: None,
                dispatch_workspace: None,
                detail_modal: crate::config::DetailModalConfig::default(),
            },
            client,
            tabs: vec![TabState {
                name: "Test".to_string(),
                jql: String::new(),
                issues: Vec::new(),
                selected: 0,
                last_fetched: None,
                last_error: None,
                tree: None,
                selected_sprint_id: None,
                sprints_cache: None,
                active_quick_filter_ids: BTreeSet::new(),
                quick_filters_cache: None,
            }],
            active_tab: 0,
            status: String::new(),
            details_visible: false,
            details_scroll: 0,
            detail_cache: HashMap::new(),
            detail_in_flight: None,
            filter: None,
            transition_picker: None,
            my_account_id: None,
            comment_editor: None,
            selection: BTreeSet::new(),
            field_picker: None,
            hide_tab_strip: false,
            board_name_cache: HashMap::new(),
            rects: Rects::default(),
            kanban_col_scroll: [0; 4],
            kanban_expanded: HashSet::new(),
            detail_modal: None,
        }
    }
}

/// Resolve a tab's `mode = ...` into a concrete JQL string, or pass
/// through a literal `jql = "..."` unchanged.
async fn resolve_tab_jql(tab: &Tab, client: &Client) -> Result<String> {
    if let Some(jql) = &tab.jql {
        return Ok(jql.clone());
    }
    // #1035 (2026-08-18) — Filter kind delegates to a Jira saved
    // filter by id. `filter = <id>` is expanded server-side to the
    // filter's saved JQL, so any query the user can view (QA queue,
    // roadmap, custom search) becomes a tab without duplicating JQL
    // into the config. `filter_id` is required for this kind (see
    // validate() below).
    if let Some(TabKind::Filter) = tab.kind {
        let id = tab
            .filter_id
            .context("kind=filter requires `filter_id = <n>`")?;
        return Ok(format!("filter = {id} ORDER BY updated DESC"));
    }
    // 2026-08-06 — honor `kind = "..."` tabs whose default_jql is a
    // static string (work_assigned / work_recently_done /
    // board_active_sprint / board_backlog). Fix-version kinds return
    // None here and fall through to the mode-based resolver below —
    // finalize() promotes their `kind` into `mode = CurrentRelease`
    // (or whatever the user set). Without this branch every
    // `kind`-only tab silently failed at query time with "neither jql
    // nor mode", even though validate() accepts it.
    if let Some(kind) = tab.kind
        && let Some(default) = kind.default_jql()
    {
        return Ok(default.to_string());
    }
    let mode = tab
        .mode
        .as_ref()
        .context("internal: tab has neither jql nor mode (should have been caught by validate)")?;
    let project = tab.project.as_ref().context("mode tab missing project")?;
    let mut versions = client
        .unreleased_versions(project)
        .await
        .context("fetching unreleased versions")?;
    // 2026-07-26 — apply the version_name_contains filter FIRST so
    // "current"/"next" picks from the filtered subset. Without
    // this, projects with multiple parallel release tracks (like
    // e.g. "Mobile - 1.6.X", "10.31.4", "13.15.0", …) pick
    // whichever version has the earliest startDate — often not
    // the one the user actually cares about.
    if let Some(needle) = &tab.version_name_contains
        && !needle.is_empty()
    {
        let needle_lower = needle.to_ascii_lowercase();
        versions.retain(|v| v.name.to_ascii_lowercase().contains(&needle_lower));
    }
    let version_name = match mode {
        ResolveMode::CurrentRelease => versions
            .first()
            .map(|v| v.name.clone())
            .context("no unreleased versions match (check version_name_contains filter)")?,
        ResolveMode::NextRelease => versions
            .get(1)
            .or_else(|| versions.first())
            .map(|v| v.name.clone())
            .context("no unreleased versions match (check version_name_contains filter)")?,
    };
    let mut jql = format!("project = {project} AND fixVersion = \"{version_name}\"");
    if let Some(c) = &tab.component {
        jql.push_str(&format!(" AND component = \"{c}\""));
    }
    jql.push_str(" ORDER BY rank");
    Ok(jql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::{Fields, Issue};

    /// Build an App skipping the async init — we don't need a real
    /// Jira client to exercise the filter logic, just `issues` +
    /// `selected` on a single tab.
    fn app_with_issues(keys_and_summaries: &[(&str, &str)]) -> App {
        let client = Client::new("https://example.atlassian.net", "x@y.z", "tok").unwrap();
        let tab = TabState {
            name: "Test".to_string(),
            jql: String::new(),
            issues: keys_and_summaries
                .iter()
                .map(|(k, s)| Issue {
                    id: String::new(),
                    key: k.to_string(),
                    fields: Fields {
                        summary: s.to_string(),
                        status: None,
                        assignee: None,
                        reporter: None,
                        priority: None,
                        issuetype: None,
                        updated: None,
                        created: None,
                        fix_versions: Vec::new(),
                        components: Vec::new(),
                        labels: Vec::new(),
                        extras: std::collections::BTreeMap::new(),
                    },
                })
                .collect(),
            selected: 0,
            last_fetched: None,
            last_error: None,
            tree: None,
            selected_sprint_id: None,
            sprints_cache: None,
            active_quick_filter_ids: BTreeSet::new(),
            quick_filters_cache: None,
        };
        App {
            cfg: Config {
                jira_url: "https://example.atlassian.net".to_string(),
                email: "x@y.z".to_string(),
                refresh_interval_secs: 60,
                tabs: Vec::new(),
                release_cut: false,
                team_field_id: None,
                team_field_name: None,
                dispatch_workspace: None,
                detail_modal: crate::config::DetailModalConfig::default(),
            },
            client,
            tabs: vec![tab],
            active_tab: 0,
            status: String::new(),
            details_visible: false,
            details_scroll: 0,
            detail_cache: HashMap::new(),
            detail_in_flight: None,
            filter: None,
            transition_picker: None,
            my_account_id: None,
            comment_editor: None,
            selection: BTreeSet::new(),
            field_picker: None,
            hide_tab_strip: false,
            board_name_cache: HashMap::new(),
            rects: Rects::default(),
            kanban_col_scroll: [0; 4],
            kanban_expanded: HashSet::new(),
            detail_modal: None,
        }
    }

    fn picker_with_transitions(keys: &[(&str, &str)]) -> TransitionPicker {
        let transitions = keys
            .iter()
            .map(|(id, name)| Transition {
                id: id.to_string(),
                name: name.to_string(),
                to_name: Some(name.to_string()),
            })
            .collect();
        TransitionPicker {
            key: "TE-1".to_string(),
            transitions: Some(transitions),
            selected: 0,
            error: None,
        }
    }

    #[test]
    fn visible_indices_with_no_filter_returns_all() {
        let app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        assert_eq!(app.visible_indices(), vec![0, 1]);
    }

    #[test]
    fn visible_indices_matches_summary_substring_case_insensitive() {
        let mut app = app_with_issues(&[
            ("TE-1", "Fix the bufferline"),
            ("TE-2", "AI panel margin"),
            ("TE-3", "Update README"),
        ]);
        app.filter = Some(FilterState {
            buffer: "PANEL".to_string(),
            cursor: 0,
            editing: false,
        });
        assert_eq!(app.visible_indices(), vec![1]);
    }

    #[test]
    fn visible_indices_matches_key_substring() {
        let mut app = app_with_issues(&[("TE-1234", "a"), ("TE-1235", "b"), ("XX-9", "te-trap")]);
        app.filter = Some(FilterState {
            buffer: "te-1234".to_string(),
            cursor: 0,
            editing: false,
        });
        assert_eq!(app.visible_indices(), vec![0]);
    }

    #[test]
    fn empty_filter_buffer_shows_all_issues() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        app.filter = Some(FilterState {
            buffer: String::new(),
            cursor: 0,
            editing: true,
        });
        assert_eq!(app.visible_indices(), vec![0, 1]);
    }

    #[test]
    fn move_selection_skips_filtered_rows() {
        let mut app = app_with_issues(&[
            ("TE-1", "alpha"),
            ("TE-2", "hidden"),
            ("TE-3", "gamma"),
            ("TE-4", "omega"),
        ]);
        app.filter = Some(FilterState {
            buffer: "a".to_string(), // matches alpha (0), gamma (2), omega (3)
            cursor: 0,
            editing: false,
        });
        assert_eq!(app.visible_indices(), vec![0, 2, 3]);
        // Start at 0 (alpha); j → 2 (gamma) — skips 1 (hidden).
        app.move_selection(1);
        assert_eq!(app.tabs[0].selected, 2);
        // j → 3 (omega).
        app.move_selection(1);
        assert_eq!(app.tabs[0].selected, 3);
        // j at the end clamps.
        app.move_selection(1);
        assert_eq!(app.tabs[0].selected, 3);
    }

    #[test]
    fn close_filter_commit_with_empty_buffer_drops_to_none() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.close_filter(FilterClose::Commit);
        assert!(app.filter.is_none());
    }

    #[test]
    fn close_filter_commit_keeps_non_empty_buffer_committed() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.filter_insert('a');
        app.close_filter(FilterClose::Commit);
        let f = app.filter.expect("commit should keep a non-empty filter");
        assert_eq!(f.buffer, "a");
        assert!(!f.editing);
    }

    #[test]
    fn close_filter_cancel_always_drops() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.filter_insert('x');
        app.close_filter(FilterClose::Cancel);
        assert!(app.filter.is_none());
    }

    #[test]
    fn filter_insert_then_backspace_round_trips() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.filter_insert('a');
        app.filter_insert('b');
        app.filter_insert('c');
        let f = app.filter.as_ref().unwrap();
        assert_eq!(f.buffer, "abc");
        assert_eq!(f.cursor, 3);
        app.filter_backspace();
        let f = app.filter.as_ref().unwrap();
        assert_eq!(f.buffer, "ab");
        assert_eq!(f.cursor, 2);
    }

    #[test]
    fn transition_picker_move_clamps_to_bounds() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(picker_with_transitions(&[
            ("11", "Start review"),
            ("21", "Mark blocked"),
            ("31", "Resolve"),
        ]));
        app.transition_picker_move(1);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 1);
        app.transition_picker_move(10);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 2);
        app.transition_picker_move(-100);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn transition_picker_select_jumps_to_index() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(picker_with_transitions(&[
            ("11", "Start review"),
            ("21", "Mark blocked"),
            ("31", "Resolve"),
        ]));
        app.transition_picker_select(2);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 2);
        // Out-of-range no-op.
        app.transition_picker_select(99);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 2);
    }

    #[test]
    fn close_transition_picker_drops_the_modal() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(picker_with_transitions(&[("11", "Resolve")]));
        app.close_transition_picker();
        assert!(app.transition_picker.is_none());
    }

    #[test]
    fn transition_picker_move_with_empty_list_is_a_no_op() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(TransitionPicker {
            key: "TE-1".to_string(),
            transitions: Some(Vec::new()),
            selected: 0,
            error: None,
        });
        app.transition_picker_move(1);
        // Stays at 0; no panic.
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn toggle_selection_adds_then_removes_focused_key() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        app.toggle_selection();
        assert!(app.selection.contains("TE-1"));
        app.toggle_selection();
        assert!(!app.selection.contains("TE-1"));
    }

    #[test]
    fn bulk_keys_returns_focused_when_selection_empty() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        app.tabs[0].selected = 1;
        assert_eq!(app.bulk_keys(), vec!["TE-2".to_string()]);
    }

    #[test]
    fn bulk_keys_returns_selection_when_non_empty() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta"), ("TE-3", "gamma")]);
        app.tabs[0].selected = 0;
        app.toggle_selection(); // selects TE-1
        app.tabs[0].selected = 2;
        app.toggle_selection(); // selects TE-3
        assert_eq!(
            app.bulk_keys(),
            vec!["TE-1".to_string(), "TE-3".to_string()] // BTreeSet sorted
        );
    }

    #[test]
    fn clear_selection_empties_the_set() {
        let mut app = app_with_issues(&[("TE-1", "a")]);
        app.toggle_selection();
        app.clear_selection();
        assert!(app.selection.is_empty());
    }

    #[test]
    fn typing_into_filter_clamps_selection_to_filtered_set() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        app.tabs[0].selected = 1; // on "beta"
        app.open_filter();
        // Type `a` — matches both "alpha" (key TE-1) and "beta" (key
        // is TE-2, but `a` ALSO matches "alpha" not "beta", so
        // visible should be just [0]). Selection should jump to 0.
        app.filter_insert('a');
        // Wait — `beta` contains `a`. Filter is summary substring
        // match — both alpha and beta match `a`. Selection should
        // stay where it is (1) since it's still in the filtered set.
        assert_eq!(app.visible_indices(), vec![0, 1]);
        assert_eq!(app.tabs[0].selected, 1);

        // Now type `lph` (so buffer = "alph"). Only alpha matches.
        app.filter_insert('l');
        app.filter_insert('p');
        app.filter_insert('h');
        assert_eq!(app.visible_indices(), vec![0]);
        // Selection clamps from 1 (beta, no longer visible) to 0.
        assert_eq!(app.tabs[0].selected, 0);
    }
}
