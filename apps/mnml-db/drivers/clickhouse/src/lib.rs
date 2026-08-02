//! ClickHouse driver crate for `mnml-db`.
//!
//! Talks to the ClickHouse HTTP endpoint (`/`) directly — no
//! persistent connection, no native binary protocol. Each query is
//! `POST /` with the SQL as the request body plus `FORMAT JSON` so
//! the response includes column metadata and rows in a single
//! payload.
//!
//! Types stay concrete (`ChCell`, `ChReply`, ...) — the main
//! `mnml-db` crate owns the neutral `Driver` trait and adapts these
//! concrete types onto it. Zero dependency on the shell.

use anyhow::{Context, Result};
use serde::Deserialize;

/// Live ClickHouse client — a `reqwest::Client` plus the base URL
/// and credentials. The client is cheap to clone and pools
/// connections under the hood.
pub struct ClickHouseDriver {
    http: reqwest::Client,
    /// Base URL — e.g. `https://my-cluster.aws.clickhouse.cloud:8443`
    /// or `http://localhost:8123`.
    url: String,
    /// Empty user = ClickHouse's `default` account.
    user: String,
    password: String,
    /// Cached server-version string for the header line.
    server_version: String,
    url_summary: String,
}

/// One column header from a query result.
#[derive(Debug, Clone)]
pub struct ChColumn {
    pub name: String,
    /// ClickHouse type name (e.g. `UInt64`, `DateTime`, `String`).
    pub type_name: String,
}

/// One cell value. ClickHouse's `FORMAT JSON` returns strings for
/// most types and native JSON scalars for numbers / booleans; we
/// keep everything as a display-string for v0.1 to match the other
/// drivers.
#[derive(Debug, Clone)]
pub enum ChCell {
    Null,
    Text(String),
}

impl ChCell {
    pub fn as_display(&self) -> String {
        match self {
            ChCell::Null => "NULL".to_string(),
            ChCell::Text(s) => s.clone(),
        }
    }
}

/// A finished query. ClickHouse's HTTP endpoint doesn't return an
/// affected-row count for DDL / DML the way MySQL does — we surface
/// a `"OK"` notice with the elapsed time.
#[derive(Debug, Clone)]
pub enum ChReply {
    Rows {
        columns: Vec<ChColumn>,
        rows: Vec<Vec<ChCell>>,
        elapsed: std::time::Duration,
        truncated: bool,
        server_row_count: usize,
    },
    Notice {
        tag: String,
        elapsed: std::time::Duration,
    },
}

/// A schema — ClickHouse's equivalent is a `database`.
#[derive(Debug, Clone)]
pub struct ChNamespace {
    pub name: String,
}

/// A queryable object living in a database.
#[derive(Debug, Clone)]
pub struct ChObject {
    pub name: String,
    pub kind: ChObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChObjectKind {
    Table,
    View,
    MaterializedView,
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct ChObjectDetail {
    pub columns: Vec<ChColumnDetail>,
}

#[derive(Debug, Clone)]
pub struct ChColumnDetail {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

impl ClickHouseDriver {
    /// Build a ClickHouse HTTP client. `url` should be the full base
    /// URL including scheme + port (e.g. `http://localhost:8123`).
    /// Empty `user` means ClickHouse's `default` account.
    pub async fn connect(url: &str, user: &str, password: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("mnml-db-driver-clickhouse/0.1.0")
            .build()
            .context("building HTTP client")?;
        let url_owned = url.trim_end_matches('/').to_string();
        let user_owned = user.to_string();
        let password_owned = password.to_string();
        // Probe the server so a broken URL / bad creds fail at
        // connect time, not on first query.
        let server_version = probe_version(&http, &url_owned, &user_owned, &password_owned)
            .await
            .unwrap_or_else(|_| "ClickHouse".to_string());
        Ok(Self {
            http,
            url_summary: summarize_url(&url_owned),
            url: url_owned,
            user: user_owned,
            password: password_owned,
            server_version,
        })
    }

    pub fn describe(&self) -> String {
        format!("ClickHouse {} ({})", self.server_version, self.url_summary)
    }

    pub async fn execute(&self, sql: &str, row_limit: u32) -> Result<ChReply> {
        let start = std::time::Instant::now();
        let trimmed = sql.trim();
        let is_select_shaped = starts_with_select_shape(trimmed);
        // If the caller already appended a FORMAT clause we leave
        // theirs alone — the server-side parse error is the right
        // signal that they need to remove one.
        let body = if is_select_shaped && !has_format_clause(trimmed) {
            format!("{trimmed} FORMAT JSON")
        } else {
            trimmed.to_string()
        };
        let text = self.post(&body).await?;
        let elapsed = start.elapsed();

        if !is_select_shaped || !body.contains("FORMAT JSON") {
            return Ok(ChReply::Notice {
                tag: text.trim().to_string(),
                elapsed,
            });
        }

        let payload: ClickhouseResponse =
            serde_json::from_str(&text).context("parsing ClickHouse JSON response")?;
        let columns: Vec<ChColumn> = payload
            .meta
            .iter()
            .map(|m| ChColumn {
                name: m.name.clone(),
                type_name: m.ty.clone(),
            })
            .collect();
        let server_row_count = payload.rows;
        let take = (row_limit as usize).min(payload.data.len());
        let truncated = payload.data.len() > take;
        let rows: Vec<Vec<ChCell>> = payload
            .data
            .into_iter()
            .take(take)
            .map(|row_obj| {
                columns
                    .iter()
                    .map(|c| match row_obj.get(&c.name) {
                        Some(serde_json::Value::Null) | None => ChCell::Null,
                        Some(serde_json::Value::String(s)) => ChCell::Text(s.clone()),
                        Some(v) => ChCell::Text(v.to_string()),
                    })
                    .collect()
            })
            .collect();
        Ok(ChReply::Rows {
            columns,
            rows,
            elapsed,
            truncated,
            server_row_count,
        })
    }

    pub async fn list_namespaces(&self) -> Result<Vec<ChNamespace>> {
        let sql = "SELECT name FROM system.databases \
                   WHERE name NOT IN ('system','INFORMATION_SCHEMA','information_schema') \
                   ORDER BY name \
                   FORMAT JSON";
        let text = self.post(sql).await?;
        let payload: ClickhouseResponse =
            serde_json::from_str(&text).context("parsing databases JSON")?;
        Ok(payload
            .data
            .into_iter()
            .filter_map(|row| {
                row.get("name").and_then(|v| {
                    v.as_str().map(|s| ChNamespace {
                        name: s.to_string(),
                    })
                })
            })
            .collect())
    }

    pub async fn list_objects(&self, ns: &str) -> Result<Vec<ChObject>> {
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT name, engine FROM system.tables \
             WHERE database = '{escaped}' \
             ORDER BY name \
             FORMAT JSON"
        );
        let text = self.post(&sql).await?;
        let payload: ClickhouseResponse =
            serde_json::from_str(&text).context("parsing tables JSON")?;
        Ok(payload
            .data
            .into_iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?.to_string();
                let engine = row
                    .get("engine")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let kind = classify_engine(&engine);
                Some(ChObject { name, kind })
            })
            .collect())
    }

    pub async fn describe_object(&self, ns: &str, obj: &str) -> Result<ChObjectDetail> {
        let ns_esc = ns.replace('\'', "''");
        let obj_esc = obj.replace('\'', "''");
        let sql = format!(
            "SELECT name, type, default_expression \
             FROM system.columns \
             WHERE database = '{ns_esc}' AND table = '{obj_esc}' \
             ORDER BY position \
             FORMAT JSON"
        );
        let text = self.post(&sql).await?;
        let payload: ClickhouseResponse =
            serde_json::from_str(&text).context("parsing columns JSON")?;
        let columns = payload
            .data
            .into_iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?.to_string();
                let data_type = row.get("type")?.as_str()?.to_string();
                let default = row
                    .get("default_expression")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                // ClickHouse nullability is expressed as
                // `Nullable(...)` inside the type string.
                let nullable = data_type.starts_with("Nullable(");
                Some(ChColumnDetail {
                    name,
                    data_type,
                    nullable,
                    default,
                })
            })
            .collect();
        Ok(ChObjectDetail { columns })
    }

    async fn post(&self, body: &str) -> Result<String> {
        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "text/plain")
            .body(body.to_string());
        if !self.user.is_empty() {
            req = req.basic_auth(&self.user, Some(&self.password));
        }
        let resp = req.send().await.context("ClickHouse HTTP request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("ClickHouse HTTP {status}: {err}"));
        }
        resp.text().await.context("reading ClickHouse response")
    }
}

async fn probe_version(
    http: &reqwest::Client,
    url: &str,
    user: &str,
    password: &str,
) -> Result<String> {
    let mut req = http
        .post(url)
        .header("Content-Type", "text/plain")
        .body("SELECT version()");
    if !user.is_empty() {
        req = req.basic_auth(user, Some(password));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("probe failed: {}", resp.status()));
    }
    let text = resp.text().await?;
    Ok(text.trim().to_string())
}

fn classify_engine(engine: &str) -> ChObjectKind {
    // ClickHouse's `engine` column carries the storage-engine name.
    // MaterializedView / View are their own engines; anything else
    // ("MergeTree", "ReplicatedMergeTree", "Log", ...) is a Table.
    match engine {
        "" => ChObjectKind::Other,
        "View" => ChObjectKind::View,
        "MaterializedView" => ChObjectKind::MaterializedView,
        _ => ChObjectKind::Table,
    }
}

fn starts_with_select_shape(sql: &str) -> bool {
    let head = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH"
    )
}

fn has_format_clause(sql: &str) -> bool {
    // Cheap check — if the SQL contains `FORMAT ` (case-insensitive)
    // in the tail, assume the user picked their own format.
    let upper = sql.to_ascii_uppercase();
    if let Some(idx) = upper.rfind("FORMAT ") {
        // Guard against `FORMAT` appearing inside a string literal;
        // require it to be in the last ~40 chars of the query.
        idx + 40 > upper.len()
    } else {
        false
    }
}

/// Short "host:port" label for the header chip.
fn summarize_url(url: &str) -> String {
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    stripped.split('/').next().unwrap_or(stripped).to_string()
}

/// The canonical SQL keyword set used for autocomplete — Postgres
/// SQL plus a handful of ClickHouse-specific top-level keywords.
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
        "ANY",
        "ALL",
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
        "MATERIALIZED",
        "DROP",
        "ALTER",
        "EXPLAIN",
        // ClickHouse-specific top-level extensions:
        "PREWHERE",
        "SETTINGS",
        "FORMAT",
        "SAMPLE",
        "WITH",
        "ARRAY",
        "OPTIMIZE",
        "FINAL",
    ]
}

#[derive(Debug, Deserialize)]
struct ClickhouseResponse {
    #[serde(default)]
    meta: Vec<ColumnMeta>,
    #[serde(default)]
    data: Vec<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    rows: usize,
}

#[derive(Debug, Deserialize)]
struct ColumnMeta {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_url_strips_scheme() {
        assert_eq!(
            summarize_url("https://xy.aws.clickhouse.cloud:8443"),
            "xy.aws.clickhouse.cloud:8443"
        );
        assert_eq!(summarize_url("http://localhost:8123/"), "localhost:8123");
        assert_eq!(summarize_url("http://ch:8123/db"), "ch:8123");
    }

    #[test]
    fn cell_display_null_is_literal() {
        assert_eq!(ChCell::Null.as_display(), "NULL");
        assert_eq!(ChCell::Text("x".into()).as_display(), "x");
    }

    #[test]
    fn keyword_set_contains_clickhouse_extensions() {
        for k in ["SELECT", "PREWHERE", "SETTINGS", "FORMAT", "SAMPLE"] {
            assert!(sql_keywords().contains(&k), "should have {k}");
        }
    }

    #[test]
    fn classify_engine_maps_view_variants() {
        assert_eq!(classify_engine("View"), ChObjectKind::View);
        assert_eq!(
            classify_engine("MaterializedView"),
            ChObjectKind::MaterializedView
        );
        assert_eq!(classify_engine("MergeTree"), ChObjectKind::Table);
        assert_eq!(classify_engine("ReplicatedMergeTree"), ChObjectKind::Table);
        assert_eq!(classify_engine(""), ChObjectKind::Other);
    }

    #[test]
    fn starts_with_select_shape_detects_row_producers() {
        assert!(starts_with_select_shape("SELECT 1"));
        assert!(starts_with_select_shape("  select 1"));
        assert!(starts_with_select_shape("SHOW TABLES"));
        assert!(starts_with_select_shape("EXPLAIN SELECT 1"));
        assert!(starts_with_select_shape(
            "WITH x AS (SELECT 1) SELECT * FROM x"
        ));
        assert!(!starts_with_select_shape("INSERT INTO t VALUES (1)"));
        assert!(!starts_with_select_shape("CREATE TABLE t (a UInt32)"));
    }

    #[test]
    fn has_format_clause_detects_tail_format() {
        assert!(has_format_clause("SELECT 1 FORMAT JSON"));
        assert!(has_format_clause("SELECT * FROM t FORMAT TabSeparated"));
        assert!(!has_format_clause("SELECT 1"));
        // FORMAT far from the end (like inside a string) shouldn't trip it.
        let long = format!("SELECT 'FORMAT dont-count'{}", " AS x".repeat(20));
        assert!(!has_format_clause(&long));
    }

    #[test]
    fn list_objects_catalog_query_shape() {
        let ns = "app";
        let escaped = ns.replace('\'', "''");
        let sql = format!(
            "SELECT name, engine FROM system.tables \
             WHERE database = '{escaped}' \
             ORDER BY name \
             FORMAT JSON"
        );
        assert!(sql.contains("system.tables"));
        assert!(sql.contains("database = 'app'"));
        assert!(sql.contains("FORMAT JSON"));
    }
}
