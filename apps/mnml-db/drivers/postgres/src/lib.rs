//! Postgres driver crate for `mnml-db`.
//!
//! Thin async wrapper around `tokio-postgres` that exposes the shape
//! the shell needs: a connection factory, a simple-query executor
//! that returns columns + row cells as strings, and schema-
//! introspection helpers.
//!
//! Types stay concrete (`PgCell`, `PgQueryResult`, ...) — the main
//! `mnml-db` crate owns the neutral `Driver` trait and adapts these
//! concrete types onto it. The driver crate has zero dependency on
//! the shell, so it can be reused elsewhere or replaced later.

use anyhow::{Context, Result};
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};

/// Live Postgres connection + a small metadata cache the shell can
/// interrogate cheaply (server version, current database).
pub struct PostgresDriver {
    client: Client,
    server_version: String,
    dsn_summary: String,
}

/// One column header from a query result.
#[derive(Debug, Clone)]
pub struct PgColumn {
    pub name: String,
    /// Best-effort type name — the simple-query protocol doesn't give
    /// us the OID, so we surface an empty string in v0.1 and fill it
    /// via `pg_type` in a later revision.
    pub type_name: String,
}

/// One cell value. The simple-query protocol returns everything as
/// text so a string is the natural in-memory shape; the shell layer
/// may reinterpret specific values (booleans, numbers) as it
/// renders.
#[derive(Debug, Clone)]
pub enum PgCell {
    Null,
    Text(String),
}

impl PgCell {
    pub fn as_display(&self) -> String {
        match self {
            PgCell::Null => "NULL".to_string(),
            PgCell::Text(s) => s.clone(),
        }
    }
}

/// A finished query — either a rowset, or a non-select statement's
/// completion tag (e.g. `INSERT 0 3`).
#[derive(Debug, Clone)]
pub enum PgReply {
    Rows {
        columns: Vec<PgColumn>,
        rows: Vec<Vec<PgCell>>,
        elapsed: std::time::Duration,
        /// True when the row cap was hit and `rows.len()` is less
        /// than what the server actually returned.
        truncated: bool,
        server_row_count: usize,
    },
    Notice {
        tag: String,
        elapsed: std::time::Duration,
    },
}

/// A schema — Postgres calls these "namespaces" internally.
#[derive(Debug, Clone)]
pub struct PgNamespace {
    pub name: String,
}

/// A queryable object living in a namespace.
#[derive(Debug, Clone)]
pub struct PgObject {
    pub name: String,
    pub kind: PgObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgObjectKind {
    Table,
    View,
    MaterializedView,
    Sequence,
    Other,
}

/// Detail block returned by `describe_object` — v0.1 populates
/// column metadata; other object kinds return an empty list.
#[derive(Debug, Clone, Default)]
pub struct PgObjectDetail {
    pub columns: Vec<PgColumnDetail>,
}

#[derive(Debug, Clone)]
pub struct PgColumnDetail {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

impl PostgresDriver {
    /// Open a connection from a Postgres DSN. Spawns the
    /// connection driver on the current tokio runtime; the caller
    /// keeps the returned handle for the life of the pane.
    pub async fn connect(dsn: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(dsn, NoTls)
            .await
            .context("connecting to Postgres")?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("mnml-db-driver-postgres: connection driver error: {e}");
            }
        });
        let server_version = client
            .simple_query("SELECT version()")
            .await
            .ok()
            .and_then(|msgs| {
                msgs.into_iter().find_map(|m| match m {
                    SimpleQueryMessage::Row(r) => {
                        r.try_get(0).ok().flatten().map(|s: &str| s.to_string())
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| "PostgreSQL".to_string());
        let dsn_summary = summarize_dsn(dsn);
        Ok(Self {
            client,
            server_version,
            dsn_summary,
        })
    }

    pub fn describe(&self) -> String {
        // "PostgreSQL 16.2 on aarch64-apple-darwin ..." can be long;
        // trim to the leading version-ish prefix so the header line
        // stays skinny.
        let short = self
            .server_version
            .split(" on ")
            .next()
            .unwrap_or(&self.server_version);
        format!("{} ({})", short, self.dsn_summary)
    }

    /// Run one SQL statement (or a batch — Postgres accepts multiple
    /// separated by `;`, but v0.1 renders only the *last* row-shaped
    /// reply). Caps at `row_limit` rows to keep an accidental
    /// `SELECT *` from a huge table from buffering forever.
    pub async fn execute(&self, sql: &str, row_limit: u32) -> Result<PgReply> {
        let start = std::time::Instant::now();
        let messages = self
            .client
            .simple_query(sql)
            .await
            .context("running query")?;
        let elapsed = start.elapsed();

        let mut columns: Vec<PgColumn> = Vec::new();
        let mut rows: Vec<Vec<PgCell>> = Vec::new();
        let mut server_row_count = 0usize;
        let mut truncated = false;
        let mut last_tag: Option<String> = None;

        for msg in messages {
            match msg {
                SimpleQueryMessage::RowDescription(cols) => {
                    columns = cols
                        .iter()
                        .map(|c| PgColumn {
                            name: c.name().to_string(),
                            type_name: String::new(),
                        })
                        .collect();
                    // A new row description arriving mid-batch means
                    // a new statement — drop prior rows to keep only
                    // the last result set.
                    rows.clear();
                    server_row_count = 0;
                    truncated = false;
                }
                SimpleQueryMessage::Row(row) => {
                    server_row_count += 1;
                    if (rows.len() as u32) < row_limit {
                        let cells: Vec<PgCell> = (0..row.len())
                            .map(|i| match row.try_get(i).ok().flatten() {
                                Some(s) => PgCell::Text(s.to_string()),
                                None => PgCell::Null,
                            })
                            .collect();
                        rows.push(cells);
                    } else {
                        truncated = true;
                    }
                }
                SimpleQueryMessage::CommandComplete(n) => {
                    last_tag = Some(format!("OK ({n})"));
                }
                _ => {}
            }
        }
        if columns.is_empty() {
            // No row description arrived — treat as a non-select.
            return Ok(PgReply::Notice {
                tag: last_tag.unwrap_or_else(|| "OK".to_string()),
                elapsed,
            });
        }
        Ok(PgReply::Rows {
            columns,
            rows,
            elapsed,
            truncated,
            server_row_count,
        })
    }

    pub async fn list_namespaces(&self) -> Result<Vec<PgNamespace>> {
        let sql = "SELECT nspname FROM pg_namespace \
                   WHERE nspname NOT LIKE 'pg_%' AND nspname <> 'information_schema' \
                   ORDER BY nspname";
        let messages = self.client.simple_query(sql).await?;
        let mut out = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg
                && let Some(name) = row.try_get(0).ok().flatten()
            {
                out.push(PgNamespace {
                    name: name.to_string(),
                });
            }
        }
        Ok(out)
    }

    pub async fn list_objects(&self, ns: &str) -> Result<Vec<PgObject>> {
        // Escape single quotes minimally — schema names shouldn't
        // contain them but be defensive.
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT c.relname, c.relkind \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = '{escaped}' \
               AND c.relkind IN ('r','v','m','S') \
             ORDER BY c.relname"
        );
        let messages = self.client.simple_query(&sql).await?;
        let mut out = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let name = row
                    .try_get(0)
                    .ok()
                    .flatten()
                    .map(|s: &str| s.to_string())
                    .unwrap_or_default();
                let kind = match row.try_get(1).ok().flatten() {
                    Some("r") => PgObjectKind::Table,
                    Some("v") => PgObjectKind::View,
                    Some("m") => PgObjectKind::MaterializedView,
                    Some("S") => PgObjectKind::Sequence,
                    _ => PgObjectKind::Other,
                };
                out.push(PgObject { name, kind });
            }
        }
        Ok(out)
    }

    pub async fn describe_object(&self, ns: &str, obj: &str) -> Result<PgObjectDetail> {
        let ns_esc = ns.replace('\'', "''");
        let obj_esc = obj.replace('\'', "''");
        let sql = format!(
            "SELECT column_name, data_type, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = '{ns_esc}' AND table_name = '{obj_esc}' \
             ORDER BY ordinal_position"
        );
        let messages = self.client.simple_query(&sql).await?;
        let mut columns = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let name = row.try_get(0).ok().flatten().unwrap_or("").to_string();
                let data_type = row.try_get(1).ok().flatten().unwrap_or("").to_string();
                let nullable = row.try_get(2).ok().flatten() == Some("YES");
                let default = row.try_get(3).ok().flatten().map(|s: &str| s.to_string());
                columns.push(PgColumnDetail {
                    name,
                    data_type,
                    nullable,
                    default,
                });
            }
        }
        Ok(PgObjectDetail { columns })
    }
}

/// The canonical SQL keyword set used for autocomplete. Kept small
/// on purpose — the shell also mixes in namespace / object names.
pub fn sql_keywords() -> &'static [&'static str] {
    &[
        "SELECT", "FROM", "WHERE", "GROUP", "ORDER", "BY", "HAVING", "LIMIT", "OFFSET", "INSERT",
        "INTO", "VALUES", "UPDATE", "SET", "DELETE", "JOIN", "INNER", "LEFT", "RIGHT", "OUTER",
        "FULL", "ON", "AND", "OR", "NOT", "IN", "IS", "NULL", "AS", "DISTINCT", "COUNT", "SUM",
        "AVG", "MIN", "MAX", "CASE", "WHEN", "THEN", "ELSE", "END", "CREATE", "TABLE", "VIEW",
        "INDEX", "DROP", "ALTER", "EXPLAIN", "ANALYZE", "VACUUM",
    ]
}

/// Trim a DSN to a short "host:port/db" label suitable for a header
/// chip. Best-effort — a malformed DSN falls back to the whole
/// thing.
fn summarize_dsn(dsn: &str) -> String {
    let Some(scheme_end) = dsn.find("://") else {
        return dsn.to_string();
    };
    let rest = &dsn[scheme_end + 3..];
    let after_userinfo = match rest.find('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    // Strip query string.
    let hostpath = after_userinfo.split('?').next().unwrap_or(after_userinfo);
    hostpath.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_dsn_pulls_host_and_db() {
        assert_eq!(
            summarize_dsn("postgresql://api:pw@db.example.com:5432/api"),
            "db.example.com:5432/api"
        );
        assert_eq!(
            summarize_dsn("postgresql://localhost:5432/postgres?sslmode=require"),
            "localhost:5432/postgres"
        );
    }

    #[test]
    fn cell_display_null_is_literal() {
        assert_eq!(PgCell::Null.as_display(), "NULL");
        assert_eq!(PgCell::Text("x".into()).as_display(), "x");
    }

    #[test]
    fn keyword_set_contains_select() {
        assert!(sql_keywords().contains(&"SELECT"));
    }
}
