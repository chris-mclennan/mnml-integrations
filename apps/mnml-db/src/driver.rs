//! The driver-neutral trait every engine implements.
//!
//! The shell (App / UI) only ever talks to `Box<dyn Driver>` — it
//! never branches on the engine name. Adding a new engine is
//! "author a driver crate + write a `impl Driver` adapter" and
//! everything downstream keeps working.
//!
//! Adapters live in `src/drivers/*.rs` and are gated by the same
//! feature flags as their crates in `Cargo.toml`.
//!
//! Some enum variants + fields on this surface (Document result
//! kind, Bytes/Json cell types, Table/Column completion kinds) are
//! stubbed for Phase 2+ drivers and unused in v0.1.
#![allow(dead_code)]

use anyhow::Result;

pub trait Driver: Send {
    /// Short one-line description shown in the header, e.g.
    /// `"PostgreSQL 16.2 (db.example.com:5432/api)"`.
    fn describe(&self) -> String;

    /// What shape the driver returns from `execute()`. The results-
    /// pane widget dispatches off this.
    fn result_kind(&self) -> ResultKind;

    /// Run one query / command. Blocking-shaped API — the driver
    /// worker thread wraps this in `tokio::task::block_in_place` +
    /// `Handle::block_on` so the shell's main render thread never
    /// blocks on I/O.
    fn execute(&mut self, q: &Query, row_limit: u32) -> Result<QueryResult>;

    /// Cancel an in-flight query. Best-effort — a no-op is fine.
    fn cancel(&mut self) {}

    /// Top-level containers: schemas for SQL, databases for Redis.
    fn list_namespaces(&mut self) -> Result<Vec<Namespace>>;

    /// Queryable / inspectable objects in a namespace.
    fn list_objects(&mut self, ns: &str) -> Result<Vec<SchemaObject>>;

    /// Detail for one object — columns for a table, TTL + peek for
    /// a Redis key.
    fn describe_object(&mut self, ns: &str, obj: &str) -> Result<ObjectDetail>;

    /// Completions for the caret position in the editor.
    fn complete(&mut self, ctx: &CompletionCtx<'_>) -> Vec<Completion>;
}

/// Everything the shell sends to a driver.
#[derive(Debug, Clone)]
pub enum Query {
    /// SQL statement or Redis command line, verbatim.
    Text(String),
}

/// What the driver returned. Distinct variants let the UI use the
/// right widget without special-casing on engine name.
#[derive(Debug, Clone)]
pub enum QueryResult {
    Rows {
        columns: Vec<Column>,
        rows: Vec<Row>,
        elapsed_ms: u128,
        truncated: bool,
        server_row_count: usize,
    },
    /// For a future document engine (Mongo / DocDB). Not produced by
    /// any Phase-1 driver but declared here so the UI widget stubs
    /// out cleanly.
    Documents {
        docs: Vec<serde_json::Value>,
        elapsed_ms: u128,
    },
    KeyValue {
        entries: Vec<KeyValueEntry>,
        elapsed_ms: u128,
        truncated: bool,
        server_row_count: usize,
    },
    /// Command tag ("OK", "PONG", "INSERT 0 3"). No table renders.
    Notice { text: String, elapsed_ms: u128 },
}

impl QueryResult {
    pub fn elapsed_ms(&self) -> u128 {
        match self {
            QueryResult::Rows { elapsed_ms, .. }
            | QueryResult::Documents { elapsed_ms, .. }
            | QueryResult::KeyValue { elapsed_ms, .. }
            | QueryResult::Notice { elapsed_ms, .. } => *elapsed_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    /// Tabular — Postgres, MariaDB, ClickHouse, Redshift.
    Rows,
    /// Tree of documents — Mongo, DocDB, DynamoDB. Not in Phase 1.
    Document,
    /// Key = value pairs — Redis.
    KeyValue,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone)]
pub struct Row(pub Vec<CellValue>);

#[derive(Debug, Clone)]
pub enum CellValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
}

impl CellValue {
    /// Best-effort one-line rendering for the results grid.
    pub fn as_display(&self) -> String {
        match self {
            CellValue::Null => "NULL".to_string(),
            CellValue::Bool(b) => b.to_string(),
            CellValue::Int(n) => n.to_string(),
            CellValue::Float(f) => f.to_string(),
            CellValue::Text(s) => s.clone(),
            CellValue::Bytes(b) => format!("<{} bytes>", b.len()),
            CellValue::Json(v) => v.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyValueEntry {
    pub key: String,
    pub value: String,
    pub type_hint: KeyValueType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyValueType {
    Nil,
    Str,
    Int,
    Bytes,
}

#[derive(Debug, Clone)]
pub struct Namespace {
    pub name: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SchemaObject {
    pub name: String,
    pub kind: ObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Sequence,
    Key,
    Stream,
    /// A document collection — Mongo / DocDB.
    Collection,
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectDetail {
    /// For SQL objects: column list.
    pub columns: Vec<ColumnDetail>,
    /// For Redis keys: TTL (seconds; -1 = persistent, -2 = missing).
    pub ttl_seconds: Option<i64>,
    /// Free-form peek at the first few values — populated for keys,
    /// left empty for tables.
    pub peek: Vec<String>,
    /// Free-form summary line ("hash · 12 fields", "table · 8 cols").
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ColumnDetail {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

pub struct CompletionCtx<'a> {
    pub text_before_cursor: &'a str,
    pub current_word: &'a str,
    pub active_namespace: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub insert: String,
    pub display: String,
    pub kind: CompletionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Table,
    Column,
    Function,
    RedisCommand,
}
