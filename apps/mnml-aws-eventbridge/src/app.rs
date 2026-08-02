//! App state — list of schedules + current selection + edit state.

use crate::config::Config;
use crate::eventbridge::{ScheduleDetail, ScheduleSummary};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

pub type PendingList = Receiver<Result<Vec<ScheduleSummary>, String>>;
pub type PendingDetail = (String, String, Receiver<Result<ScheduleDetail, String>>);
pub type PendingSave = Receiver<Result<(), String>>;
/// (name, group, result) — one tuple per schedule as the prefetch
/// thread walks the list.
pub type PrefetchStream = Receiver<(String, String, Result<ScheduleDetail, String>)>;

pub struct App {
    pub cfg: Config,
    pub schedules: Vec<ScheduleSummary>,
    pub selected: usize,
    /// Loaded on demand for the highlighted row.
    pub detail: Option<ScheduleDetail>,
    pub detail_loading: bool,
    /// Backfilled by the prefetch thread so the list rows can show
    /// each row's Schedule expression + a human-readable
    /// translation without waiting for row-by-row lazy fetches.
    /// Keyed by (name, group_name).
    pub detail_cache: HashMap<(String, String), ScheduleDetail>,
    /// (name, group_name) tuples the user has expanded to see
    /// full detail. Rows are collapsed by default.
    pub expanded: HashSet<(String, String)>,
    /// Rows the mouse handler can hit-test — populated each frame
    /// by the renderer as (schedule_index, y_start, y_end) inside
    /// the pane's terminal coords.
    pub row_hits: Vec<(usize, u16, u16)>,
    /// Whole-list scroll offset in rows (each expanded schedule
    /// takes multiple rows).
    pub scroll_offset: u16,
    pub status: String,
    pub last_error: Option<String>,
    pub mode: Mode,
    pub edit: Edit,
    pub pending_list: Option<PendingList>,
    pub pending_detail: Option<PendingDetail>,
    pub pending_prefetch: Option<PrefetchStream>,
    pub pending_save: Option<PendingSave>,
    pub should_quit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Edit,
    Saving,
}

/// Buffered fields the user is editing. Tab cycles focus.
/// `expression_cursor` / `input_cursor` are byte offsets into the
/// respective strings; the renderer walks them to place the
/// terminal cursor and the key handler mutates the string at
/// those offsets (insert / delete / navigate).
#[derive(Debug, Clone, Default)]
pub struct Edit {
    pub focus: EditFocus,
    pub expression: String,
    pub expression_cursor: usize,
    pub input: String,
    pub input_cursor: usize,
}

impl Edit {
    /// (text, cursor) for whichever field currently has focus.
    pub fn focused(&self) -> (&str, usize) {
        match self.focus {
            EditFocus::Expression => (&self.expression, self.expression_cursor),
            EditFocus::Input => (&self.input, self.input_cursor),
        }
    }

    pub fn text_mut(&mut self) -> &mut String {
        match self.focus {
            EditFocus::Expression => &mut self.expression,
            EditFocus::Input => &mut self.input,
        }
    }

    pub fn cursor_mut(&mut self) -> &mut usize {
        match self.focus {
            EditFocus::Expression => &mut self.expression_cursor,
            EditFocus::Input => &mut self.input_cursor,
        }
    }

    /// Move the focused cursor left one char (respecting UTF-8
    /// boundaries).
    pub fn move_left(&mut self) {
        let (text, cursor) = self.focused();
        if cursor == 0 {
            return;
        }
        let mut i = cursor - 1;
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        *self.cursor_mut() = i;
    }

    pub fn move_right(&mut self) {
        let (text, cursor) = self.focused();
        if cursor >= text.len() {
            return;
        }
        let mut i = cursor + 1;
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        *self.cursor_mut() = i;
    }

    /// Home: for the expression field jump to 0. For the multi-line
    /// input field jump to the start of the current line.
    pub fn move_home(&mut self) {
        match self.focus {
            EditFocus::Expression => self.expression_cursor = 0,
            EditFocus::Input => {
                self.input_cursor = self.input[..self.input_cursor]
                    .rfind('\n')
                    .map(|i| i + 1)
                    .unwrap_or(0);
            }
        }
    }

    /// End: for the expression field jump to len. For the multi-line
    /// input field jump to the end of the current line.
    pub fn move_end(&mut self) {
        match self.focus {
            EditFocus::Expression => self.expression_cursor = self.expression.len(),
            EditFocus::Input => {
                let rest = &self.input[self.input_cursor..];
                self.input_cursor += rest.find('\n').unwrap_or(rest.len());
            }
        }
    }

    /// Up/Down: multi-line navigation for the input field. Preserves
    /// the visual column when possible. No-op for the expression
    /// field (which is a single line).
    pub fn move_up(&mut self) {
        if !matches!(self.focus, EditFocus::Input) {
            return;
        }
        let col = self.visual_col();
        let line_start = self.input[..self.input_cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        if line_start == 0 {
            self.input_cursor = 0;
            return;
        }
        let prev_end = line_start - 1;
        let prev_start = self.input[..prev_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line = &self.input[prev_start..prev_end];
        let target_col = col.min(prev_line.chars().count());
        let target_byte: usize = prev_line
            .char_indices()
            .nth(target_col)
            .map(|(b, _)| b)
            .unwrap_or(prev_line.len());
        self.input_cursor = prev_start + target_byte;
    }

    pub fn move_down(&mut self) {
        if !matches!(self.focus, EditFocus::Input) {
            return;
        }
        let col = self.visual_col();
        let rest = &self.input[self.input_cursor..];
        let Some(nl) = rest.find('\n') else {
            self.input_cursor = self.input.len();
            return;
        };
        let next_start = self.input_cursor + nl + 1;
        let next_rest = &self.input[next_start..];
        let next_line_len = next_rest.find('\n').unwrap_or(next_rest.len());
        let next_line = &next_rest[..next_line_len];
        let target_col = col.min(next_line.chars().count());
        let target_byte: usize = next_line
            .char_indices()
            .nth(target_col)
            .map(|(b, _)| b)
            .unwrap_or(next_line.len());
        self.input_cursor = next_start + target_byte;
    }

    /// Character column of the focused cursor within its current
    /// line (chars — not bytes, not display width).
    pub fn visual_col(&self) -> usize {
        let (text, cursor) = self.focused();
        let line_start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
        text[line_start..cursor].chars().count()
    }

    /// Insert a character at the cursor and advance.
    pub fn insert_char(&mut self, c: char) {
        let cursor = *self.cursor_mut();
        self.text_mut().insert(cursor, c);
        *self.cursor_mut() = cursor + c.len_utf8();
    }

    /// Insert a literal newline at the cursor (Enter in the JSON
    /// field only — the expression field is single-line).
    pub fn insert_newline(&mut self) {
        if !matches!(self.focus, EditFocus::Input) {
            return;
        }
        self.insert_char('\n');
    }

    /// Backspace — delete the char before the cursor.
    pub fn backspace(&mut self) {
        let cursor = *self.cursor_mut();
        if cursor == 0 {
            return;
        }
        let text = self.text_mut();
        let mut i = cursor - 1;
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        text.replace_range(i..cursor, "");
        *self.cursor_mut() = i;
    }

    /// Delete — remove the char at the cursor.
    pub fn delete_forward(&mut self) {
        let cursor = *self.cursor_mut();
        let text = self.text_mut();
        if cursor >= text.len() {
            return;
        }
        let mut end = cursor + 1;
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        text.replace_range(cursor..end, "");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditFocus {
    #[default]
    Expression,
    Input,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        let mut app = App {
            cfg,
            schedules: Vec::new(),
            selected: 0,
            detail: None,
            detail_loading: false,
            detail_cache: HashMap::new(),
            expanded: HashSet::new(),
            row_hits: Vec::new(),
            scroll_offset: 0,
            status: "loading schedules…".to_string(),
            last_error: None,
            mode: Mode::Browse,
            edit: Edit::default(),
            pending_list: None,
            pending_detail: None,
            pending_prefetch: None,
            pending_save: None,
            should_quit: false,
        };
        app.refresh_list();
        app
    }

    pub fn selected_key(&self) -> Option<(String, String)> {
        self.schedules
            .get(self.selected)
            .map(|s| (s.name.clone(), s.group_name.clone()))
    }

    /// Toggle the expanded/collapsed state at a specific index —
    /// used by the mouse handler which knows the row it clicked.
    pub fn toggle_expand_at(&mut self, idx: usize) {
        let Some(s) = self.schedules.get(idx) else {
            return;
        };
        let key = (s.name.clone(), s.group_name.clone());
        if self.expanded.contains(&key) {
            self.expanded.remove(&key);
        } else {
            self.expanded.insert(key);
        }
    }

    pub fn expand_selected(&mut self) {
        if let Some(key) = self.selected_key() {
            self.expanded.insert(key);
        }
    }

    pub fn collapse_selected(&mut self) {
        if let Some(key) = self.selected_key() {
            self.expanded.remove(&key);
        }
    }

    pub fn refresh_list(&mut self) {
        let region = self.cfg.region.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r =
                crate::eventbridge::list_schedules(region.as_deref()).map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.pending_list = Some(rx);
        self.detail_cache.clear();
        // Drop the in-flight prefetch stream so its late results
        // don't land in the freshly-cleared cache and confuse the
        // next render. The background thread's send will fail
        // silently — thread exits, thread state gc'd (2026-07-22
        // tester finding).
        self.pending_prefetch = None;
        self.pending_detail = None;
        self.status = "refreshing…".to_string();
    }

    /// After the list arrives, walk it sequentially and populate
    /// `detail_cache` so list rows can show each row's expression
    /// alongside a humanized translation. Sequential (not
    /// parallel) to stay under any per-account Scheduler
    /// throttling.
    pub fn start_prefetch(&mut self) {
        let region = self.cfg.region.clone();
        let items: Vec<(String, String)> = self
            .schedules
            .iter()
            .map(|s| (s.name.clone(), s.group_name.clone()))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (name, group) in items {
                let r = crate::eventbridge::get_schedule(&name, &group, region.as_deref())
                    .map_err(|e| e.to_string());
                if tx.send((name, group, r)).is_err() {
                    // Receiver dropped (app quit / new refresh) — abort.
                    break;
                }
            }
        });
        self.pending_prefetch = Some(rx);
    }

    pub fn refresh_detail(&mut self) {
        let Some(sum) = self.schedules.get(self.selected).cloned() else {
            return;
        };
        // Cache hit — populate detail immediately, skip the fetch.
        if let Some(cached) = self.detail_cache.get(&(sum.name.clone(), sum.group_name.clone())) {
            self.detail = Some(cached.clone());
            self.detail_loading = false;
            return;
        }
        let region = self.cfg.region.clone();
        let name = sum.name.clone();
        let group = sum.group_name.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let name_t = name.clone();
        let group_t = group.clone();
        std::thread::spawn(move || {
            let r = crate::eventbridge::get_schedule(&name_t, &group_t, region.as_deref())
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.pending_detail = Some((name, group, rx));
        self.detail_loading = true;
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.schedules.len();
        if n == 0 {
            return;
        }
        // Clamp (not wrap) — PageDown at the last row shouldn't
        // teleport to the top. Vim canonical + expected behavior;
        // was `rem_euclid` (wrap) which the tester flagged as
        // surprising 2026-07-22.
        let s = (self.selected as isize + delta).clamp(0, n as isize - 1) as usize;
        self.selected = s;
        self.detail = None;
        self.refresh_detail();
    }

    pub fn enter_edit(&mut self) {
        let Some(d) = self.detail.clone() else {
            self.status = "load a schedule first (wait for detail)".to_string();
            return;
        };
        let expression = d.schedule_expression.clone();
        let input = d.target.input.clone().unwrap_or_default();
        let expression_cursor = expression.len();
        let input_cursor = input.len();
        self.edit = Edit {
            focus: EditFocus::Expression,
            expression,
            expression_cursor,
            input,
            input_cursor,
        };
        self.mode = Mode::Edit;
        self.status = "editing — Tab switches field · Ctrl+S save · Esc cancel".to_string();
    }

    pub fn cancel_edit(&mut self) {
        self.mode = Mode::Browse;
        self.status = format!("cancelled edit of {}", self.selected_name());
    }

    /// Toggle the highlighted schedule's state (ENABLED ⇄ DISABLED).
    /// Uses the same background-thread pattern as `save_edit`; the
    /// selected row's detail cache entry gets refreshed after.
    pub fn toggle_state(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let Some(d) = self.detail_cache.get(&key).cloned() else {
            self.status = "load detail first (wait for prefetch)".to_string();
            return;
        };
        let region = self.cfg.region.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = crate::eventbridge::toggle_state(&d, region.as_deref())
                .map_err(|e| e.to_string());
            let _ = tx.send(r.map(|_| ()));
        });
        self.pending_save = Some(rx);
        self.mode = Mode::Saving;
        self.status = format!("toggling {}…", key.0);
    }

    pub fn save_edit(&mut self) {
        let Some(d) = self.detail.clone() else {
            return;
        };
        let region = self.cfg.region.clone();
        let expr = self.edit.expression.clone();
        let input = self.edit.input.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = crate::eventbridge::update_schedule(&d, &expr, &input, region.as_deref())
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.pending_save = Some(rx);
        self.mode = Mode::Saving;
        self.status = "saving…".to_string();
    }

    pub fn selected_name(&self) -> String {
        self.schedules
            .get(self.selected)
            .map(|s| s.name.clone())
            .unwrap_or_default()
    }

    /// Drain pending background fetches. Called each tick.
    pub fn poll_background(&mut self) -> bool {
        let mut any = false;
        if let Some(rx) = &self.pending_list {
            match rx.try_recv() {
                Ok(Ok(list)) => {
                    self.schedules = list;
                    self.selected = self.selected.min(self.schedules.len().saturating_sub(1));
                    self.status = format!("{} schedules", self.schedules.len());
                    self.pending_list = None;
                    self.detail = None;
                    self.refresh_detail();
                    self.start_prefetch();
                    any = true;
                }
                Ok(Err(e)) => {
                    self.last_error = Some(e.clone());
                    self.status = format!("list error: {e}");
                    self.pending_list = None;
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_list = None;
                }
            }
        }
        if let Some((name, group, rx)) = &self.pending_detail {
            match rx.try_recv() {
                Ok(Ok(detail)) => {
                    if self
                        .schedules
                        .get(self.selected)
                        .is_some_and(|s| &s.name == name && &s.group_name == group)
                    {
                        self.detail = Some(detail);
                    }
                    self.detail_loading = false;
                    self.pending_detail = None;
                    any = true;
                }
                Ok(Err(e)) => {
                    self.last_error = Some(e.clone());
                    self.status = format!("detail error: {e}");
                    self.detail_loading = false;
                    self.pending_detail = None;
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_detail = None;
                }
            }
        }
        // Drain the prefetch stream. Collect first, mutate self
        // after — the loop can't hold `&self.pending_prefetch` and
        // also mutate `self.detail` / `self.detail_cache`.
        let mut batch: Vec<(String, String, ScheduleDetail)> = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &self.pending_prefetch {
            loop {
                match rx.try_recv() {
                    Ok((name, group, Ok(detail))) => batch.push((name, group, detail)),
                    Ok((_, _, Err(_))) => {
                        // Skip per-row errors so one bad row doesn't
                        // clobber the status line.
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.pending_prefetch = None;
        }
        if !batch.is_empty() {
            any = true;
            for (name, group, detail) in batch {
                if self.detail.is_none()
                    && self
                        .schedules
                        .get(self.selected)
                        .is_some_and(|s| s.name == name && s.group_name == group)
                {
                    self.detail = Some(detail.clone());
                    self.detail_loading = false;
                }
                self.detail_cache.insert((name, group), detail);
            }
        }
        if let Some(rx) = &self.pending_save {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.mode = Mode::Browse;
                    self.status = format!("saved {}", self.selected_name());
                    self.pending_save = None;
                    // Invalidate the cached detail for this row so
                    // refresh_detail actually re-fetches instead of
                    // serving the stale (pre-save) copy.
                    if let Some(key) = self.selected_key() {
                        self.detail_cache.remove(&key);
                    }
                    self.detail = None;
                    self.refresh_detail();
                    // Also invalidate the summary — state / target
                    // ARN on the list row need to reflect the update.
                    // list-schedules is cheap; just re-run it.
                    self.refresh_list();
                    any = true;
                }
                Ok(Err(e)) => {
                    self.last_error = Some(e.clone());
                    self.status = format!("save error: {e}");
                    self.mode = Mode::Edit;
                    self.pending_save = None;
                    any = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_save = None;
                }
            }
        }
        any
    }
}
