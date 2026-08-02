//! App state — connection list, active connection, focus, and the
//! driver-worker channel.
//!
//! Async model: each connection gets a dedicated worker thread. The
//! worker owns the `Box<dyn Driver>` and blocks on driver calls;
//! the render thread only ever sends requests down an `mpsc` and
//! drains responses per frame. This is the shape needed to keep
//! ratatui's event loop responsive across long-running queries.

use anyhow::Result;
use std::sync::Arc;
use std::sync::mpsc;
use tokio::runtime::Handle;
use tokio::sync::Mutex as AsyncMutex;

use crate::config::Config;
use crate::connection::ConnectionSpec;
use crate::driver::{
    Completion, CompletionCtx, Namespace, ObjectDetail, Query, QueryResult, ResultKind,
    SchemaObject,
};
use crate::drivers;
use crate::history::History;

/// One connection's runtime state — includes the worker channel
/// once the connection has been opened.
pub struct ConnState {
    pub spec: ConnectionSpec,
    pub worker: Option<Worker>,
    /// Cached engine describe(). None until connected.
    pub describe: Option<String>,
    /// Last error surfaced by the worker.
    pub last_error: Option<String>,
    /// Schema tree cache.
    pub schema: SchemaCache,
    /// Per-connection history.
    pub history: History,
}

impl ConnState {
    pub fn new(spec: ConnectionSpec) -> Self {
        let history = History::for_connection(&spec.id)
            .unwrap_or_else(|_| History::for_connection("_fallback").unwrap());
        Self {
            spec,
            worker: None,
            describe: None,
            last_error: None,
            schema: SchemaCache::default(),
            history,
        }
    }

    pub fn result_kind(&self) -> Option<ResultKind> {
        self.worker.as_ref().map(|w| w.result_kind)
    }
}

/// Schema-tree cache. Populated lazily as the user expands nodes.
#[derive(Default)]
pub struct SchemaCache {
    pub namespaces: Option<Vec<Namespace>>,
    /// namespace name → object list.
    pub objects: std::collections::BTreeMap<String, Vec<SchemaObject>>,
    /// (namespace, object) → detail.
    pub details: std::collections::BTreeMap<(String, String), ObjectDetail>,
}

/// Channel + JoinHandle for one connection's driver worker.
pub struct Worker {
    pub tx: mpsc::Sender<DriverRequest>,
    pub rx: mpsc::Receiver<DriverResponse>,
    pub result_kind: ResultKind,
    pub describe: String,
    #[allow(dead_code)]
    pub thread: std::thread::JoinHandle<()>,
}

#[allow(dead_code)] // DescribeObject + Shutdown are wired for v0.2 (schema-detail popup).
pub enum DriverRequest {
    Execute { query: Query, row_limit: u32 },
    ListNamespaces,
    ListObjects { namespace: String },
    DescribeObject { namespace: String, object: String },
    Complete { ctx: OwnedCompletionCtx },
    Shutdown,
}

pub struct OwnedCompletionCtx {
    pub text_before_cursor: String,
    pub current_word: String,
    pub active_namespace: Option<String>,
}

pub enum DriverResponse {
    Executed(Result<QueryResult>),
    Namespaces(Result<Vec<Namespace>>),
    Objects {
        namespace: String,
        result: Result<Vec<SchemaObject>>,
    },
    ObjectDetail {
        namespace: String,
        object: String,
        result: Result<ObjectDetail>,
    },
    Completions(Vec<Completion>),
}

/// Focus target for keybinding routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    SchemaTree,
    Editor,
    Results,
    ConnPicker,
    HistoryPicker,
    ObjectPicker,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    /// Active connection picker.
    ConnPicker {
        index: usize,
    },
    /// Recall picker — a list of history entries.
    HistoryPicker {
        entries: Vec<String>,
        index: usize,
    },
    /// Schema-object picker for jumping to a table / key.
    ObjectPicker {
        candidates: Vec<PickerObject>,
        query: String,
        index: usize,
    },
    /// Completion popup anchored on the editor caret.
    Completion {
        completions: Vec<Completion>,
        index: usize,
    },
}

#[derive(Debug, Clone)]
pub struct PickerObject {
    pub namespace: String,
    pub name: String,
}

pub struct App {
    #[allow(dead_code)] // captured for v0.2 (config-driven column-width, retry policy)
    pub cfg: Config,
    pub connections: Vec<ConnState>,
    pub active: Option<usize>,
    pub editor: EditorState,
    pub result: Option<QueryResult>,
    pub result_row: usize,
    pub result_filter: String,
    pub status: String,
    pub focus: Focus,
    pub overlay: Overlay,
    pub row_limit: u32,
    pub should_quit: bool,
    /// Schema-tree UI state — which namespace is expanded.
    pub tree: TreeState,
    /// Document-result expanded-path set. Keyed by the path notation
    /// used by `ui::results_tree` (`"3"`, `"3.address"`, `"3.tags[1]"`).
    /// Reset whenever a new document result arrives.
    pub doc_expanded: std::collections::BTreeSet<String>,
    pub runtime: Handle,
}

#[derive(Default)]
pub struct TreeState {
    pub selected: usize,
    pub expanded: std::collections::BTreeSet<String>,
    /// Flat, ordered list of visible tree lines (rebuilt each render).
    pub visible: Vec<TreeLine>,
}

#[derive(Debug, Clone)]
pub enum TreeLine {
    Namespace(String),
    Object { namespace: String, name: String },
}

pub struct EditorState {
    pub text: String,
    pub cursor: usize,
    /// Scroll offset for the editor pane — wired but unused until
    /// the v0.2 rewrap pass; kept live for shape.
    #[allow(dead_code)]
    pub scroll_y: usize,
}

impl EditorState {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            scroll_y: 0,
        }
    }

    pub fn insert(&mut self, c: char) {
        let byte = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.text.len());
        self.text.insert(byte, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self
            .text
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(b, _)| b)
            .unwrap_or(0);
        let end = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.text.len());
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn newline(&mut self) {
        self.insert('\n');
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.text = text;
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.chars().count() {
            self.cursor += 1;
        }
    }

    /// Current word up to the cursor (used for completion).
    pub fn current_word(&self) -> String {
        let text: String = self.text.chars().take(self.cursor).collect();
        text.rsplit(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// The full statement / command line covering the caret. For
    /// multi-line SQL this is the semicolon-delimited chunk; for a
    /// single-line Redis command it's the whole text.
    pub fn statement_at_cursor(&self) -> String {
        // Byte-offset of char-index cursor.
        let byte = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len());
        crate::query::statement_at_cursor(&self.text, byte)
    }
}

impl App {
    pub fn new(cfg: Config, runtime: Handle) -> Self {
        let row_limit = cfg.row_limit;
        let connections: Vec<ConnState> = cfg
            .connections
            .iter()
            .cloned()
            .map(ConnState::new)
            .collect();
        let active = if connections.is_empty() {
            None
        } else {
            Some(0)
        };
        Self {
            cfg,
            connections,
            active,
            editor: EditorState::new(),
            result: None,
            result_row: 0,
            result_filter: String::new(),
            status: "press Ctrl+Enter to run · Ctrl+K switch conn · Tab cycle focus · q quit"
                .to_string(),
            focus: Focus::Editor,
            overlay: Overlay::None,
            row_limit,
            should_quit: false,
            tree: TreeState::default(),
            doc_expanded: Default::default(),
            runtime,
        }
    }

    pub fn active_conn(&self) -> Option<&ConnState> {
        self.active.and_then(|i| self.connections.get(i))
    }

    pub fn active_conn_mut(&mut self) -> Option<&mut ConnState> {
        self.active.and_then(|i| self.connections.get_mut(i))
    }

    #[allow(dead_code)] // reserved for the outline-panel title in v0.2
    pub fn active_describe(&self) -> String {
        match self.active_conn() {
            Some(c) => match &c.describe {
                Some(d) => format!("{} · {}", c.spec.display_label(), d),
                None => format!(
                    "{} · {} (not connected)",
                    c.spec.display_label(),
                    c.spec.engine
                ),
            },
            None => "(no connection)".into(),
        }
    }

    /// Open the worker for the active connection if not already.
    pub fn ensure_worker(&mut self) -> Result<()> {
        let Some(i) = self.active else {
            return Err(anyhow::anyhow!("no active connection"));
        };
        if self.connections[i].worker.is_some() {
            return Ok(());
        }
        let spec = self.connections[i].spec.clone();
        let runtime = self.runtime.clone();
        let worker = spawn_worker(spec, runtime)?;
        self.connections[i].describe = Some(worker.describe.clone());
        self.connections[i].last_error = None;
        self.connections[i].worker = Some(worker);
        self.request_namespaces();
        Ok(())
    }

    pub fn switch_connection(&mut self, idx: usize) {
        if idx >= self.connections.len() {
            return;
        }
        self.active = Some(idx);
        self.status = format!("connection: {}", self.connections[idx].spec.display_label());
        self.result = None;
        self.result_row = 0;
        self.tree = TreeState::default();
    }

    pub fn run_query(&mut self) {
        let stmt = {
            let editor = &self.editor;
            let s = editor.statement_at_cursor();
            if s.trim().is_empty() {
                editor.text.trim().to_string()
            } else {
                s
            }
        };
        if stmt.trim().is_empty() {
            self.status = "query is empty".to_string();
            return;
        }
        if let Err(e) = self.ensure_worker() {
            // tester 2026-07-31 SEV-1 — `{e}` prints only the outer
            // anyhow context frame ("connecting to Postgres"), losing
            // the real root cause ("Connection refused"). `{e:#}`
            // walks the chain.
            self.status = format!("connect failed: {e:#}");
            if let Some(c) = self.active_conn_mut() {
                c.last_error = Some(e.to_string());
            }
            return;
        }
        let row_limit = self.row_limit;
        let stmt_clone = stmt.clone();
        let Some(worker) = self.active_conn_mut().and_then(|c| c.worker.as_ref()) else {
            self.status = "no worker".to_string();
            return;
        };
        if let Err(e) = worker.tx.send(DriverRequest::Execute {
            query: Query::Text(stmt),
            row_limit,
        }) {
            self.status = format!("send failed: {e}");
            return;
        }
        self.status = format!("running · {}…", truncate_status(&stmt_clone, 60));
        if let Some(c) = self.active_conn_mut() {
            let _ = c.history.record(&stmt_clone);
        }
    }

    pub fn run_all(&mut self) {
        if self.editor.text.trim().is_empty() {
            self.status = "editor is empty".to_string();
            return;
        }
        // Snapshot the cursor, park it at the end, run, restore.
        let saved = self.editor.cursor;
        self.editor.cursor = self.editor.text.chars().count();
        self.run_query();
        self.editor.cursor = saved;
    }

    pub fn request_namespaces(&mut self) {
        if let Some(c) = self.active_conn_mut()
            && let Some(w) = c.worker.as_ref()
        {
            let _ = w.tx.send(DriverRequest::ListNamespaces);
        }
    }

    pub fn request_objects(&mut self, ns: &str) {
        if let Some(c) = self.active_conn_mut()
            && let Some(w) = c.worker.as_ref()
        {
            let _ = w.tx.send(DriverRequest::ListObjects {
                namespace: ns.to_string(),
            });
        }
    }

    pub fn request_completions(&mut self) {
        let (before, word, ns) = {
            let e = &self.editor;
            let before: String = e.text.chars().take(e.cursor).collect();
            let word = e.current_word();
            let ns = self
                .active_conn()
                .and_then(|c| c.schema.namespaces.as_ref())
                .and_then(|n| n.first().map(|n| n.name.clone()));
            (before, word, ns)
        };
        if let Some(c) = self.active_conn_mut()
            && let Some(w) = c.worker.as_ref()
        {
            let _ = w.tx.send(DriverRequest::Complete {
                ctx: OwnedCompletionCtx {
                    text_before_cursor: before,
                    current_word: word,
                    active_namespace: ns,
                },
            });
        }
    }

    /// Drain any pending responses across all connections. Returns
    /// `true` if any response was applied (so the caller can redraw
    /// immediately instead of waiting for the next tick).
    pub fn drain(&mut self) -> bool {
        let mut any = false;
        for i in 0..self.connections.len() {
            // Collect first, apply second — try_recv() borrows the
            // channel immutably, and apply_response() takes &mut self.
            let mut pending: Vec<DriverResponse> = Vec::new();
            if let Some(w) = self.connections[i].worker.as_ref() {
                while let Ok(resp) = w.rx.try_recv() {
                    pending.push(resp);
                }
            }
            for resp in pending {
                any = true;
                self.apply_response(i, resp);
            }
        }
        any
    }

    fn apply_response(&mut self, conn_idx: usize, resp: DriverResponse) {
        let is_active = self.active == Some(conn_idx);
        match resp {
            DriverResponse::Executed(r) => match r {
                Ok(result) => {
                    let (kind, ms, rows, truncated, server_row_count) = describe_result(&result);
                    self.status = if let Some(rows) = rows {
                        if truncated {
                            format!(
                                "{rows} of {} rows · {ms}ms · truncated (R to double limit)",
                                server_row_count.unwrap_or(rows)
                            )
                        } else {
                            format!("{rows} rows · {ms}ms · {kind}")
                        }
                    } else {
                        format!("{kind} · {ms}ms")
                    };
                    if is_active {
                        self.result = Some(result);
                        self.result_row = 0;
                        self.doc_expanded.clear();
                        self.focus = Focus::Results;
                    }
                }
                Err(e) => {
                    self.status = format!("error: {e:#}");
                    self.connections[conn_idx].last_error = Some(e.to_string());
                    if is_active {
                        self.result = None;
                    }
                }
            },
            DriverResponse::Namespaces(r) => match r {
                Ok(list) => self.connections[conn_idx].schema.namespaces = Some(list),
                Err(e) => self.connections[conn_idx].last_error = Some(e.to_string()),
            },
            DriverResponse::Objects { namespace, result } => match result {
                Ok(list) => {
                    self.connections[conn_idx]
                        .schema
                        .objects
                        .insert(namespace, list);
                }
                Err(e) => self.connections[conn_idx].last_error = Some(e.to_string()),
            },
            DriverResponse::ObjectDetail {
                namespace,
                object,
                result,
            } => {
                if let Ok(d) = result {
                    self.connections[conn_idx]
                        .schema
                        .details
                        .insert((namespace, object), d);
                }
            }
            DriverResponse::Completions(list) => {
                if !list.is_empty() && is_active {
                    self.overlay = Overlay::Completion {
                        completions: list,
                        index: 0,
                    };
                }
            }
        }
    }

    pub fn move_result_row(&mut self, delta: isize) {
        let Some(result) = self.result.as_ref() else {
            return;
        };
        let n = match result {
            QueryResult::Rows { rows, .. } => rows.len(),
            QueryResult::KeyValue { entries, .. } => entries.len(),
            // Docs cursor walks the current *visible* line list so
            // expanded object / array children are reachable.
            QueryResult::Documents { docs, .. } => {
                crate::ui::results_tree::build_lines(docs, &self.doc_expanded).len()
            }
            QueryResult::Notice { .. } => 0,
        };
        if n == 0 {
            return;
        }
        let s = self.result_row as isize + delta;
        self.result_row = s.clamp(0, n as isize - 1) as usize;
    }

    pub fn cycle_focus(&mut self, reverse: bool) {
        use Focus::*;
        let seq = [SchemaTree, Editor, Results];
        let cur = seq.iter().position(|f| *f == self.focus).unwrap_or(1);
        let next = if reverse {
            (cur + seq.len() - 1) % seq.len()
        } else {
            (cur + 1) % seq.len()
        };
        self.focus = seq[next];
    }

    pub fn double_row_limit(&mut self) {
        self.row_limit = self.row_limit.saturating_mul(2);
        self.status = format!("row_limit = {} — re-run with Ctrl+Enter", self.row_limit);
    }
}

fn describe_result(r: &QueryResult) -> (&'static str, u128, Option<usize>, bool, Option<usize>) {
    match r {
        QueryResult::Rows {
            rows,
            elapsed_ms,
            truncated,
            server_row_count,
            ..
        } => (
            "rows",
            *elapsed_ms,
            Some(rows.len()),
            *truncated,
            Some(*server_row_count),
        ),
        QueryResult::KeyValue {
            entries,
            elapsed_ms,
            truncated,
            server_row_count,
        } => (
            "keyvalue",
            *elapsed_ms,
            Some(entries.len()),
            *truncated,
            Some(*server_row_count),
        ),
        QueryResult::Documents {
            docs, elapsed_ms, ..
        } => ("docs", *elapsed_ms, Some(docs.len()), false, None),
        QueryResult::Notice { elapsed_ms, .. } => ("notice", *elapsed_ms, None, false, None),
    }
}

fn truncate_status(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Spawn a driver worker thread. The thread owns the boxed driver
/// and blocks on driver calls; the shell keeps the mpsc endpoints.
///
/// Every response the worker sends is derived from a request the
/// shell sent, so the response ordering is well-defined.
fn spawn_worker(spec: ConnectionSpec, runtime: Handle) -> Result<Worker> {
    let (req_tx, req_rx) = mpsc::channel::<DriverRequest>();
    let (resp_tx, resp_rx) = mpsc::channel::<DriverResponse>();
    // Open the driver on the main runtime so `Handle::current()` in
    // the adapter resolves.
    //
    // tester 2026-07-31 SEV-2 — was an unbounded block_on that
    // froze the whole TUI (event loop lives on this thread) if the
    // host was firewalled / black-holed. Cap at 8s: fits the common
    // slow-TLS-handshake case (~2-4s for a fresh Redshift cluster)
    // while surfacing a real timeout instead of hanging forever.
    // Better fix would be moving connect entirely to the worker
    // thread with progress reporting, but this keeps the API shape
    // and unblocks the UX bug now.
    const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
    let driver = runtime.block_on(async {
        tokio::time::timeout(CONNECT_TIMEOUT, drivers::connect(&spec))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "connect timed out after {}s — check host/port/firewall",
                    CONNECT_TIMEOUT.as_secs()
                )
            })?
    })?;
    let describe = driver.describe();
    let result_kind = driver.result_kind();
    let driver = Arc::new(AsyncMutex::new(driver));
    let driver_worker = driver.clone();

    // Move the runtime handle into the worker so it can drive async
    // driver calls via `Handle::block_on`.
    let handle = runtime.clone();
    let thread = std::thread::Builder::new()
        .name(format!("mnml-db-worker[{}]", spec.id))
        .spawn(move || {
            let _guard = handle.enter();
            while let Ok(req) = req_rx.recv() {
                match req {
                    DriverRequest::Execute { query, row_limit } => {
                        let mut d = driver_worker.blocking_lock();
                        let out = d.execute(&query, row_limit);
                        let _ = resp_tx.send(DriverResponse::Executed(out));
                    }
                    DriverRequest::ListNamespaces => {
                        let mut d = driver_worker.blocking_lock();
                        let out = d.list_namespaces();
                        let _ = resp_tx.send(DriverResponse::Namespaces(out));
                    }
                    DriverRequest::ListObjects { namespace } => {
                        let mut d = driver_worker.blocking_lock();
                        let out = d.list_objects(&namespace);
                        let _ = resp_tx.send(DriverResponse::Objects {
                            namespace,
                            result: out,
                        });
                    }
                    DriverRequest::DescribeObject { namespace, object } => {
                        let mut d = driver_worker.blocking_lock();
                        let out = d.describe_object(&namespace, &object);
                        let _ = resp_tx.send(DriverResponse::ObjectDetail {
                            namespace,
                            object,
                            result: out,
                        });
                    }
                    DriverRequest::Complete { ctx } => {
                        let mut d = driver_worker.blocking_lock();
                        let out = d.complete(&CompletionCtx {
                            text_before_cursor: &ctx.text_before_cursor,
                            current_word: &ctx.current_word,
                            active_namespace: ctx.active_namespace.as_deref(),
                        });
                        let _ = resp_tx.send(DriverResponse::Completions(out));
                    }
                    DriverRequest::Shutdown => break,
                }
            }
        })?;

    Ok(Worker {
        tx: req_tx,
        rx: resp_rx,
        result_kind,
        describe,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_insert_and_backspace() {
        let mut e = EditorState::new();
        e.insert_str("SELECT");
        assert_eq!(e.text, "SELECT");
        assert_eq!(e.cursor, 6);
        e.backspace();
        assert_eq!(e.text, "SELEC");
        assert_eq!(e.cursor, 5);
    }

    #[test]
    fn editor_current_word_walks_back_to_boundary() {
        let mut e = EditorState::new();
        e.insert_str("SELECT * FROM us");
        assert_eq!(e.current_word(), "us");
    }

    #[test]
    fn editor_statement_at_cursor_uses_semicolons() {
        let mut e = EditorState::new();
        e.insert_str("SELECT 1; SELECT 2");
        // Cursor at end.
        assert_eq!(e.statement_at_cursor(), "SELECT 2");
    }

    #[test]
    fn truncate_status_appends_ellipsis() {
        assert_eq!(truncate_status("abcdef", 3), "abc…");
        assert_eq!(truncate_status("ab", 5), "ab");
    }
}
