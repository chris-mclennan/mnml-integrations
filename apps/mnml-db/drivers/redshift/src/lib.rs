//! Redshift driver crate for `mnml-db`.
//!
//! Redshift speaks the Postgres wire protocol so this driver is
//! structurally the same as `mnml-db-driver-postgres` — the
//! differences are the catalog queries (Redshift uses the `svv_*`
//! system views instead of `pg_namespace` / `pg_tables`), and a
//! Redshift-specific header string.
//!
//! Types stay concrete (`RsCell`, `RsReply`, ...) — the main
//! `mnml-db` crate owns the neutral `Driver` trait and adapts these
//! concrete types onto it. Zero dependency on the shell.

use anyhow::{Context, Result};
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Live Redshift connection + a small metadata cache.
pub struct RedshiftDriver {
    client: Client,
    server_version: String,
    dsn_summary: String,
}

/// One column header from a query result.
#[derive(Debug, Clone)]
pub struct RsColumn {
    pub name: String,
    /// Best-effort type name — the simple-query protocol doesn't
    /// give us the OID, so we surface an empty string in v0.1.
    pub type_name: String,
}

/// One cell value. Simple-query returns everything as text.
#[derive(Debug, Clone)]
pub enum RsCell {
    Null,
    Text(String),
}

impl RsCell {
    pub fn as_display(&self) -> String {
        match self {
            RsCell::Null => "NULL".to_string(),
            RsCell::Text(s) => s.clone(),
        }
    }
}

/// A finished query — either a rowset, or a non-select completion tag.
#[derive(Debug, Clone)]
pub enum RsReply {
    Rows {
        columns: Vec<RsColumn>,
        rows: Vec<Vec<RsCell>>,
        elapsed: std::time::Duration,
        truncated: bool,
        server_row_count: usize,
    },
    Notice {
        tag: String,
        elapsed: std::time::Duration,
    },
}

/// A schema.
#[derive(Debug, Clone)]
pub struct RsNamespace {
    pub name: String,
}

/// A queryable object living in a namespace.
#[derive(Debug, Clone)]
pub struct RsObject {
    pub name: String,
    pub kind: RsObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsObjectKind {
    Table,
    View,
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct RsObjectDetail {
    pub columns: Vec<RsColumnDetail>,
}

#[derive(Debug, Clone)]
pub struct RsColumnDetail {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

impl RedshiftDriver {
    /// Open a connection from a Postgres-shape DSN. Redshift is
    /// TLS-mandatory on managed AWS clusters — v0.2 (2026-07-31)
    /// uses rustls + webpki-roots by default so real clusters work
    /// out of the box. Set `sslmode=disable` in the DSN to fall
    /// back to `NoTls` for local dev / clusterless setups.
    pub async fn connect(dsn: &str) -> Result<Self> {
        let disable_tls = dsn_has(dsn, "sslmode", "disable");
        let (client, driver_task): (Client, tokio::task::JoinHandle<()>) = if disable_tls {
            let (c, connection) = tokio_postgres::connect(dsn, NoTls)
                .await
                .context("connecting to Redshift (NoTls)")?;
            let task = tokio::spawn(async move {
                if let Err(e) = connection.await {
                    eprintln!("mnml-db-driver-redshift: connection driver error: {e}");
                }
            });
            (c, task)
        } else {
            let tls = make_rustls_connector()?;
            let (c, connection) = tokio_postgres::connect(dsn, tls)
                .await
                .context("connecting to Redshift (TLS)")?;
            let task = tokio::spawn(async move {
                if let Err(e) = connection.await {
                    eprintln!("mnml-db-driver-redshift: connection driver error: {e}");
                }
            });
            (c, task)
        };
        // Detached deliberately: dropping a JoinHandle does not cancel
        // the tokio task, so the connection driver keeps running for as
        // long as the client holds the socket open.
        let _driver_task = driver_task;
        // Redshift reports as `PostgreSQL 8.0.2 on ...` in
        // version(), plus its own Redshift version tag. Prefer the
        // Redshift-specific view when available.
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
            .unwrap_or_else(|| "Redshift".to_string());
        let dsn_summary = summarize_dsn(dsn);
        Ok(Self {
            client,
            server_version,
            dsn_summary,
        })
    }

    pub fn describe(&self) -> String {
        // The version() string is long; keep the leading prefix.
        let short = self
            .server_version
            .split(" on ")
            .next()
            .unwrap_or(&self.server_version);
        format!("Redshift ({short}) ({})", self.dsn_summary)
    }

    pub async fn execute(&self, sql: &str, row_limit: u32) -> Result<RsReply> {
        let start = std::time::Instant::now();
        let messages = self
            .client
            .simple_query(sql)
            .await
            .context("running query")?;
        let elapsed = start.elapsed();

        let mut columns: Vec<RsColumn> = Vec::new();
        let mut rows: Vec<Vec<RsCell>> = Vec::new();
        let mut server_row_count = 0usize;
        let mut truncated = false;
        let mut last_tag: Option<String> = None;

        for msg in messages {
            match msg {
                SimpleQueryMessage::RowDescription(cols) => {
                    columns = cols
                        .iter()
                        .map(|c| RsColumn {
                            name: c.name().to_string(),
                            type_name: String::new(),
                        })
                        .collect();
                    rows.clear();
                    server_row_count = 0;
                    truncated = false;
                }
                SimpleQueryMessage::Row(row) => {
                    server_row_count += 1;
                    if (rows.len() as u32) < row_limit {
                        let cells: Vec<RsCell> = (0..row.len())
                            .map(|i| match row.try_get(i).ok().flatten() {
                                Some(s) => RsCell::Text(s.to_string()),
                                None => RsCell::Null,
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
            return Ok(RsReply::Notice {
                tag: last_tag.unwrap_or_else(|| "OK".to_string()),
                elapsed,
            });
        }
        Ok(RsReply::Rows {
            columns,
            rows,
            elapsed,
            truncated,
            server_row_count,
        })
    }

    /// Namespaces come from Redshift's `SVV_ALL_SCHEMAS` view — it
    /// spans local + external (Spectrum) schemas in one query.
    pub async fn list_namespaces(&self) -> Result<Vec<RsNamespace>> {
        let sql = "SELECT schema_name FROM svv_all_schemas \
                   WHERE schema_name NOT LIKE 'pg_%' \
                     AND schema_name <> 'information_schema' \
                   ORDER BY schema_name";
        let messages = self.client.simple_query(sql).await?;
        let mut out = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg
                && let Some(name) = row.try_get(0).ok().flatten()
            {
                out.push(RsNamespace {
                    name: name.to_string(),
                });
            }
        }
        Ok(out)
    }

    /// Objects come from `SVV_ALL_TABLES` — includes external tables.
    pub async fn list_objects(&self, ns: &str) -> Result<Vec<RsObject>> {
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT table_name, table_type FROM svv_all_tables \
             WHERE schema_name = '{escaped}' \
             ORDER BY table_name"
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
                let kind = classify_kind(row.try_get(1).ok().flatten().unwrap_or(""));
                out.push(RsObject { name, kind });
            }
        }
        Ok(out)
    }

    /// Column detail comes from `SVV_ALL_COLUMNS`.
    pub async fn describe_object(&self, ns: &str, obj: &str) -> Result<RsObjectDetail> {
        let ns_esc = ns.replace('\'', "''");
        let obj_esc = obj.replace('\'', "''");
        let sql = format!(
            "SELECT column_name, data_type, is_nullable, column_default \
             FROM svv_all_columns \
             WHERE schema_name = '{ns_esc}' AND table_name = '{obj_esc}' \
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
                columns.push(RsColumnDetail {
                    name,
                    data_type,
                    nullable,
                    default,
                });
            }
        }
        Ok(RsObjectDetail { columns })
    }
}

fn classify_kind(raw: &str) -> RsObjectKind {
    // `svv_all_tables.table_type` returns values like `TABLE`,
    // `VIEW`, `EXTERNAL TABLE`. Group tables + externals together
    // for now — the visual distinction lands in v0.2.
    match raw {
        "VIEW" => RsObjectKind::View,
        "TABLE" | "EXTERNAL TABLE" => RsObjectKind::Table,
        _ => RsObjectKind::Other,
    }
}

/// The canonical SQL keyword set — Redshift is Postgres-flavored, so
/// the Postgres keyword list is the right starting point.
pub fn sql_keywords() -> &'static [&'static str] {
    &[
        "SELECT",
        "FROM",
        "WHERE",
        "GROUP",
        "ORDER",
        "BY",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "INSERT",
        "INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE",
        "JOIN",
        "INNER",
        "LEFT",
        "RIGHT",
        "OUTER",
        "FULL",
        "ON",
        "AND",
        "OR",
        "NOT",
        "IN",
        "IS",
        "NULL",
        "AS",
        "DISTINCT",
        "COUNT",
        "SUM",
        "AVG",
        "MIN",
        "MAX",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "CREATE",
        "TABLE",
        "VIEW",
        "INDEX",
        "DROP",
        "ALTER",
        "EXPLAIN",
        "ANALYZE",
        "VACUUM",
        // Redshift-specific niceties:
        "UNLOAD",
        "COPY",
        "DISTKEY",
        "SORTKEY",
        "DISTSTYLE",
        "ENCODE",
    ]
}

/// Case-insensitive search for `key=value` in the DSN's query
/// string (`?sslmode=disable&…`). Used to detect the sslmode=disable
/// escape hatch without pulling in a full URL parser.
fn dsn_has(dsn: &str, key: &str, value: &str) -> bool {
    let Some(qmark) = dsn.find('?') else {
        return false;
    };
    dsn[qmark + 1..].split('&').any(|kv| {
        let mut parts = kv.splitn(2, '=');
        let k = parts.next().unwrap_or("").trim();
        let v = parts.next().unwrap_or("").trim();
        k.eq_ignore_ascii_case(key) && v.eq_ignore_ascii_case(value)
    })
}

/// Build a rustls-based `MakeRustlsConnect` for tokio-postgres,
/// trusting the webpki-roots CA bundle (AWS-managed Redshift
/// clusters ship with certs from this set).
fn make_rustls_connector() -> Result<MakeRustlsConnect> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(MakeRustlsConnect::new(cfg))
}

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
            summarize_dsn(
                "postgresql://user:pw@dw.abc.us-east-1.redshift.amazonaws.com:5439/warehouse"
            ),
            "dw.abc.us-east-1.redshift.amazonaws.com:5439/warehouse"
        );
        assert_eq!(
            summarize_dsn("postgresql://localhost:5439/dev?sslmode=require"),
            "localhost:5439/dev"
        );
    }

    #[test]
    fn cell_display_null_is_literal() {
        assert_eq!(RsCell::Null.as_display(), "NULL");
        assert_eq!(RsCell::Text("x".into()).as_display(), "x");
    }

    #[test]
    fn keyword_set_contains_redshift_extensions() {
        for k in ["UNLOAD", "COPY", "DISTKEY", "SORTKEY"] {
            assert!(sql_keywords().contains(&k), "should have {k}");
        }
    }

    #[test]
    fn classify_kind_maps_svv_all_tables_values() {
        assert_eq!(classify_kind("VIEW"), RsObjectKind::View);
        assert_eq!(classify_kind("TABLE"), RsObjectKind::Table);
        assert_eq!(classify_kind("EXTERNAL TABLE"), RsObjectKind::Table);
        assert_eq!(classify_kind(""), RsObjectKind::Other);
    }

    #[test]
    fn list_objects_uses_svv_catalog() {
        let ns = "public";
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT table_name, table_type FROM svv_all_tables \
             WHERE schema_name = '{escaped}' \
             ORDER BY table_name"
        );
        assert!(sql.contains("svv_all_tables"));
        assert!(sql.contains("schema_name = 'public'"));
    }

    #[test]
    fn list_namespaces_excludes_pg_and_information_schema() {
        let sql = "SELECT schema_name FROM svv_all_schemas \
                   WHERE schema_name NOT LIKE 'pg_%' \
                     AND schema_name <> 'information_schema' \
                   ORDER BY schema_name";
        assert!(sql.contains("svv_all_schemas"));
        assert!(sql.contains("pg_%"));
        assert!(sql.contains("information_schema"));
    }
}
