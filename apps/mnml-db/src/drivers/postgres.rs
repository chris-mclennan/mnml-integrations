//! Postgres adapter — wraps `mnml_db_driver_postgres::PostgresDriver`
//! in the neutral `Driver` trait.
//!
//! Async driver calls are exposed as sync methods by driving them
//! through the current tokio runtime (`Handle::block_on`). The
//! driver worker thread is guaranteed to have a runtime handle
//! attached (see `src/app.rs`).

use anyhow::{Context, Result};
use tokio::runtime::Handle;

use mnml_db_driver_postgres::{PgCell, PgObjectKind, PgReply, PostgresDriver, sql_keywords};

use crate::connection::{ConnectionSpec, resolve_password};
use crate::driver::{
    CellValue, Column, Completion, CompletionCtx, CompletionKind, Driver, Namespace, ObjectDetail,
    ObjectKind, Query, QueryResult, ResultKind, Row, SchemaObject,
};

pub struct PgAdapter {
    inner: PostgresDriver,
    description: String,
}

impl PgAdapter {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let dsn = build_dsn(spec)?;
        let inner = PostgresDriver::connect(&dsn).await?;
        let description = inner.describe();
        Ok(Self { inner, description })
    }
}

impl Driver for PgAdapter {
    fn describe(&self) -> String {
        self.description.clone()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Rows
    }

    fn execute(&mut self, q: &Query, row_limit: u32) -> Result<QueryResult> {
        let Query::Text(sql) = q;
        let reply = Handle::current().block_on(self.inner.execute(sql, row_limit))?;
        Ok(convert_reply(reply))
    }

    fn list_namespaces(&mut self) -> Result<Vec<Namespace>> {
        let out = Handle::current().block_on(self.inner.list_namespaces())?;
        Ok(out
            .into_iter()
            .map(|n| Namespace {
                name: n.name,
                label: None,
            })
            .collect())
    }

    fn list_objects(&mut self, ns: &str) -> Result<Vec<SchemaObject>> {
        let out = Handle::current().block_on(self.inner.list_objects(ns))?;
        Ok(out
            .into_iter()
            .map(|o| SchemaObject {
                name: o.name,
                kind: match o.kind {
                    PgObjectKind::Table => ObjectKind::Table,
                    PgObjectKind::View => ObjectKind::View,
                    PgObjectKind::MaterializedView => ObjectKind::MaterializedView,
                    PgObjectKind::Sequence => ObjectKind::Sequence,
                    PgObjectKind::Other => ObjectKind::Other,
                },
            })
            .collect())
    }

    fn describe_object(&mut self, ns: &str, obj: &str) -> Result<ObjectDetail> {
        let detail = Handle::current().block_on(self.inner.describe_object(ns, obj))?;
        let summary = if detail.columns.is_empty() {
            None
        } else {
            Some(format!("table · {} cols", detail.columns.len()))
        };
        Ok(ObjectDetail {
            columns: detail
                .columns
                .into_iter()
                .map(|c| crate::driver::ColumnDetail {
                    name: c.name,
                    data_type: c.data_type,
                    nullable: c.nullable,
                    default: c.default,
                })
                .collect(),
            ttl_seconds: None,
            peek: Vec::new(),
            summary,
        })
    }

    fn complete(&mut self, ctx: &CompletionCtx<'_>) -> Vec<Completion> {
        let prefix = ctx.current_word.to_ascii_uppercase();
        sql_keywords()
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .map(|k| Completion {
                insert: (*k).to_string(),
                display: (*k).to_string(),
                kind: CompletionKind::Keyword,
            })
            .collect()
    }
}

fn convert_reply(reply: PgReply) -> QueryResult {
    match reply {
        PgReply::Rows {
            columns,
            rows,
            elapsed,
            truncated,
            server_row_count,
        } => {
            let cols: Vec<Column> = columns
                .into_iter()
                .map(|c| Column {
                    name: c.name,
                    type_name: c.type_name,
                })
                .collect();
            let rows: Vec<Row> = rows
                .into_iter()
                .map(|r| {
                    Row(r
                        .into_iter()
                        .map(|c| match c {
                            PgCell::Null => CellValue::Null,
                            PgCell::Text(s) => CellValue::Text(s),
                        })
                        .collect())
                })
                .collect();
            QueryResult::Rows {
                columns: cols,
                rows,
                elapsed_ms: elapsed.as_millis(),
                truncated,
                server_row_count,
            }
        }
        PgReply::Notice { tag, elapsed } => QueryResult::Notice {
            text: tag,
            elapsed_ms: elapsed.as_millis(),
        },
    }
}

/// Build a Postgres DSN from the neutral ConnectionSpec — resolves
/// the credential source into inline `user:pass` inside the URL.
/// Never logged; the caller (main) surfaces a redacted summary via
/// `PostgresDriver::describe()`.
fn build_dsn(spec: &ConnectionSpec) -> Result<String> {
    let password = resolve_password(spec).context("resolving password")?;
    let user = spec.user.as_deref().unwrap_or("postgres");
    let host = spec.host.as_deref().unwrap_or("localhost");
    let port = spec.port.unwrap_or(5432);
    let db = spec.database.as_deref().unwrap_or("postgres");
    // Percent-encode the password just enough to keep `@` / `:` /
    // `/` out of the userinfo segment; a real vendored encoder is
    // overkill for v0.1 since credentials shouldn't contain these.
    let enc = |s: &str| {
        s.chars()
            .map(|c| match c {
                '@' => "%40".to_string(),
                ':' => "%3A".to_string(),
                '/' => "%2F".to_string(),
                other => other.to_string(),
            })
            .collect::<String>()
    };
    let userinfo = if password.is_empty() {
        enc(user)
    } else {
        format!("{}:{}", enc(user), enc(&password))
    };
    Ok(format!("postgresql://{userinfo}@{host}:{port}/{db}"))
}
