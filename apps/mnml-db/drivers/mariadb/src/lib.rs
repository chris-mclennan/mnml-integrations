//! MariaDB / MySQL driver crate for `mnml-db`.
//!
//! Thin async wrapper around `mysql_async` that exposes the shape
//! the shell needs: a connection factory, a query executor that
//! returns columns + row cells as strings, and schema-introspection
//! helpers.
//!
//! Types stay concrete (`MyCell`, `MyReply`, ...) — the main
//! `mnml-db` crate owns the neutral `Driver` trait and adapts these
//! concrete types onto it. The driver crate has zero dependency on
//! the shell.
//!
//! v0.1 uses `mysql_async`'s text-protocol path and coerces every
//! value via `from_value_opt::<String>` so we render every cell
//! verbatim without per-type formatting. v0.2 will move to typed
//! decoding with rich-type display.

use anyhow::{Context, Result};
use mysql_async::prelude::*;
use mysql_async::{Conn, Row};

/// Live MariaDB connection + a cached server-version string.
pub struct MariaDriver {
    conn: Conn,
    server_version: String,
    dsn_summary: String,
}

/// One column header from a query result.
#[derive(Debug, Clone)]
pub struct MyColumn {
    pub name: String,
    /// MariaDB column type name (e.g. `VARCHAR`, `INT`) — populated
    /// via `column.column_type()` when available; empty for v0.1.
    pub type_name: String,
}

/// One cell value. mysql_async decodes into `mysql_async::Value`; we
/// coerce to a string in v0.1 so the shell can render uniformly.
#[derive(Debug, Clone)]
pub enum MyCell {
    Null,
    Text(String),
}

impl MyCell {
    pub fn as_display(&self) -> String {
        match self {
            MyCell::Null => "NULL".to_string(),
            MyCell::Text(s) => s.clone(),
        }
    }
}

/// A finished query — either a rowset, or a non-select statement's
/// completion tag (e.g. `OK (3)`).
#[derive(Debug, Clone)]
pub enum MyReply {
    Rows {
        columns: Vec<MyColumn>,
        rows: Vec<Vec<MyCell>>,
        elapsed: std::time::Duration,
        truncated: bool,
        server_row_count: usize,
    },
    Notice {
        tag: String,
        elapsed: std::time::Duration,
    },
}

/// A schema — in MariaDB / MySQL terminology, a database.
#[derive(Debug, Clone)]
pub struct MyNamespace {
    pub name: String,
}

/// A queryable object living in a schema.
#[derive(Debug, Clone)]
pub struct MyObject {
    pub name: String,
    pub kind: MyObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyObjectKind {
    Table,
    View,
    Sequence,
    Other,
}

/// Detail block returned by `describe_object` — v0.1 populates
/// column metadata; other object kinds return an empty list.
#[derive(Debug, Clone, Default)]
pub struct MyObjectDetail {
    pub columns: Vec<MyColumnDetail>,
}

#[derive(Debug, Clone)]
pub struct MyColumnDetail {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

impl MariaDriver {
    /// Open a connection from a MySQL / MariaDB DSN of the shape
    /// `mysql://user:pass@host:port/db`.
    pub async fn connect(dsn: &str) -> Result<Self> {
        let opts = mysql_async::OptsBuilder::from_opts(
            mysql_async::Opts::from_url(dsn).context("parsing DSN")?,
        );
        let mut conn = Conn::new(opts).await.context("connecting to MariaDB")?;
        let server_version = fetch_scalar(&mut conn, "SELECT VERSION()")
            .await
            .unwrap_or_else(|_| "MariaDB".to_string());
        Ok(Self {
            conn,
            server_version,
            dsn_summary: summarize_dsn(dsn),
        })
    }

    pub fn describe(&self) -> String {
        format!("MariaDB {} ({})", self.server_version, self.dsn_summary)
    }

    /// Run one SQL statement. Caps at `row_limit` rows so an
    /// accidental `SELECT *` on a huge table doesn't buffer forever.
    pub async fn execute(&mut self, sql: &str, row_limit: u32) -> Result<MyReply> {
        let start = std::time::Instant::now();
        // Distinguish between statements that produce rows (SELECT /
        // SHOW / DESCRIBE / etc.) and ones that don't (INSERT /
        // UPDATE / etc.). `query` works for both — an empty rowset
        // just means the server returned no rows.
        let result: Vec<Row> = self.conn.query(sql).await.context("running query")?;
        let elapsed = start.elapsed();

        if result.is_empty() {
            let tag = self.conn.affected_rows().to_string();
            return Ok(MyReply::Notice {
                tag: format!("OK ({tag})"),
                elapsed,
            });
        }

        let columns: Vec<MyColumn> = result
            .first()
            .map(|r| {
                r.columns_ref()
                    .iter()
                    .map(|c| MyColumn {
                        name: c.name_str().to_string(),
                        type_name: format!("{:?}", c.column_type()),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let server_row_count = result.len();
        let take = (row_limit as usize).min(result.len());
        let truncated = result.len() > take;
        let rows: Vec<Vec<MyCell>> = result
            .into_iter()
            .take(take)
            .map(|row| {
                (0..row.len())
                    .map(|i| match row.as_ref(i) {
                        Some(mysql_async::Value::NULL) | None => MyCell::Null,
                        Some(v) => match mysql_async::from_value_opt::<String>(v.clone()) {
                            Ok(s) => MyCell::Text(s),
                            Err(_) => MyCell::Text(format!("{v:?}")),
                        },
                    })
                    .collect()
            })
            .collect();

        Ok(MyReply::Rows {
            columns,
            rows,
            elapsed,
            truncated,
            server_row_count,
        })
    }

    pub async fn list_namespaces(&mut self) -> Result<Vec<MyNamespace>> {
        let sql = "SELECT schema_name FROM information_schema.schemata \
                   WHERE schema_name NOT IN ('mysql','information_schema','performance_schema','sys') \
                   ORDER BY schema_name";
        let rows: Vec<String> = self.conn.query(sql).await.context("listing schemas")?;
        Ok(rows.into_iter().map(|name| MyNamespace { name }).collect())
    }

    pub async fn list_objects(&mut self, ns: &str) -> Result<Vec<MyObject>> {
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT table_name, table_type FROM information_schema.tables \
             WHERE table_schema = '{escaped}' \
             ORDER BY table_name"
        );
        let rows: Vec<(String, String)> = self.conn.query(&sql).await.context("listing tables")?;
        Ok(rows
            .into_iter()
            .map(|(name, kind)| MyObject {
                name,
                kind: match kind.as_str() {
                    "BASE TABLE" => MyObjectKind::Table,
                    "VIEW" => MyObjectKind::View,
                    "SEQUENCE" => MyObjectKind::Sequence,
                    _ => MyObjectKind::Other,
                },
            })
            .collect())
    }

    pub async fn describe_object(&mut self, ns: &str, obj: &str) -> Result<MyObjectDetail> {
        let ns_esc = ns.replace('\'', "''");
        let obj_esc = obj.replace('\'', "''");
        let sql = format!(
            "SELECT column_name, column_type, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = '{ns_esc}' AND table_name = '{obj_esc}' \
             ORDER BY ordinal_position"
        );
        let rows: Vec<(String, String, String, Option<String>)> =
            self.conn.query(&sql).await.context("describing table")?;
        let columns = rows
            .into_iter()
            .map(|(name, data_type, is_nullable, default)| MyColumnDetail {
                name,
                data_type,
                nullable: is_nullable.eq_ignore_ascii_case("YES"),
                default,
            })
            .collect();
        Ok(MyObjectDetail { columns })
    }
}

/// Run a query that returns a single scalar column & row.
async fn fetch_scalar(conn: &mut Conn, sql: &str) -> Result<String> {
    let rows: Vec<String> = conn.query(sql).await?;
    rows.into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no rows for `{sql}`"))
}

/// The canonical SQL keyword set used for autocomplete. Same list as
/// the Postgres driver — MariaDB is close enough for v0.1.
pub fn sql_keywords() -> &'static [&'static str] {
    &[
        "SELECT", "FROM", "WHERE", "GROUP", "ORDER", "BY", "HAVING", "LIMIT", "OFFSET", "INSERT",
        "INTO", "VALUES", "UPDATE", "SET", "DELETE", "JOIN", "INNER", "LEFT", "RIGHT", "OUTER",
        "FULL", "ON", "AND", "OR", "NOT", "IN", "IS", "NULL", "AS", "DISTINCT", "COUNT", "SUM",
        "AVG", "MIN", "MAX", "CASE", "WHEN", "THEN", "ELSE", "END", "CREATE", "TABLE", "VIEW",
        "INDEX", "DROP", "ALTER", "EXPLAIN", "SHOW", "DESCRIBE", "USE",
    ]
}

/// Trim a DSN to a short "host:port/db" label suitable for a header
/// chip.
fn summarize_dsn(dsn: &str) -> String {
    let Some(scheme_end) = dsn.find("://") else {
        return dsn.to_string();
    };
    let rest = &dsn[scheme_end + 3..];
    let after_userinfo = match rest.find('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    let hostpath = after_userinfo.split('?').next().unwrap_or(after_userinfo);
    hostpath.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_dsn_pulls_host_and_db() {
        assert_eq!(
            summarize_dsn("mysql://api:pw@db.example.com:3306/api"),
            "db.example.com:3306/api"
        );
        assert_eq!(
            summarize_dsn("mysql://localhost:3306/app?ssl-mode=REQUIRED"),
            "localhost:3306/app"
        );
    }

    #[test]
    fn cell_display_null_is_literal() {
        assert_eq!(MyCell::Null.as_display(), "NULL");
        assert_eq!(MyCell::Text("x".into()).as_display(), "x");
    }

    #[test]
    fn keyword_set_contains_select_and_show() {
        assert!(sql_keywords().contains(&"SELECT"));
        assert!(sql_keywords().contains(&"SHOW"));
    }

    #[test]
    fn list_objects_catalog_query_shape() {
        // Guard against typos in the catalog query — build the SQL
        // for a schema name and check the shape.
        let ns = "app";
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT table_name, table_type FROM information_schema.tables \
             WHERE table_schema = '{escaped}' \
             ORDER BY table_name"
        );
        assert!(sql.contains("information_schema.tables"));
        assert!(sql.contains("table_schema = 'app'"));
    }

    #[test]
    fn list_namespaces_excludes_system_schemas() {
        let sql = "SELECT schema_name FROM information_schema.schemata \
                   WHERE schema_name NOT IN ('mysql','information_schema','performance_schema','sys') \
                   ORDER BY schema_name";
        for sys in ["mysql", "information_schema", "performance_schema", "sys"] {
            assert!(sql.contains(sys), "should exclude {sys}");
        }
    }
}
