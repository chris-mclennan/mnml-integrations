//! ClickHouse adapter — wraps
//! `mnml_db_driver_clickhouse::ClickHouseDriver` in the neutral
//! `Driver` trait.

use anyhow::{Context, Result};
use tokio::runtime::Handle;

use mnml_db_driver_clickhouse::{ChCell, ChObjectKind, ChReply, ClickHouseDriver, sql_keywords};

use crate::connection::{ConnectionSpec, resolve_password};
use crate::driver::{
    CellValue, Column, Completion, CompletionCtx, CompletionKind, Driver, Namespace, ObjectDetail,
    ObjectKind, Query, QueryResult, ResultKind, Row, SchemaObject,
};

pub struct ClickHouseAdapter {
    inner: ClickHouseDriver,
    description: String,
}

impl ClickHouseAdapter {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let (url, user, password) = build_endpoint(spec)?;
        let inner = ClickHouseDriver::connect(&url, &user, &password).await?;
        let description = inner.describe();
        Ok(Self { inner, description })
    }
}

impl Driver for ClickHouseAdapter {
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
                    ChObjectKind::Table => ObjectKind::Table,
                    ChObjectKind::View => ObjectKind::View,
                    ChObjectKind::MaterializedView => ObjectKind::MaterializedView,
                    ChObjectKind::Other => ObjectKind::Other,
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

fn convert_reply(reply: ChReply) -> QueryResult {
    match reply {
        ChReply::Rows {
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
                            ChCell::Null => CellValue::Null,
                            ChCell::Text(s) => CellValue::Text(s),
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
        ChReply::Notice { tag, elapsed } => QueryResult::Notice {
            text: tag,
            elapsed_ms: elapsed.as_millis(),
        },
    }
}

/// Build a ClickHouse HTTP endpoint + creds from the neutral spec.
/// Scheme defaults to `http` (port 8123 = plaintext ClickHouse); set
/// `params.scheme = "https"` for TLS clusters (which also switch the
/// default port to 8443 unless overridden).
fn build_endpoint(spec: &ConnectionSpec) -> Result<(String, String, String)> {
    let password = resolve_password(spec).context("resolving password")?;
    let scheme = spec
        .params
        .get("scheme")
        .map(String::as_str)
        .unwrap_or("http");
    let host = spec.host.as_deref().unwrap_or("localhost");
    let port = spec
        .port
        .unwrap_or(if scheme.eq_ignore_ascii_case("https") {
            8443
        } else {
            8123
        });
    let user = spec.user.clone().unwrap_or_default();
    // ClickHouse routes queries to a specific database via a
    // `database` URL query param; the fastest, most standard shape.
    let url = if let Some(db) = spec.database.as_deref() {
        format!("{scheme}://{host}:{port}/?database={db}")
    } else {
        format!("{scheme}://{host}:{port}")
    };
    Ok((url, user, password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::CredsSource;

    fn spec_with(
        user: Option<&str>,
        host: Option<&str>,
        port: Option<u16>,
        db: Option<&str>,
    ) -> ConnectionSpec {
        ConnectionSpec {
            id: "test".into(),
            label: None,
            engine: "clickhouse".into(),
            host: host.map(str::to_string),
            port,
            user: user.map(str::to_string),
            database: db.map(str::to_string),
            params: Default::default(),
            creds: None,
        }
    }

    #[test]
    fn build_endpoint_defaults_to_http_8123() {
        let s = spec_with(None, None, None, None);
        let (url, user, pass) = build_endpoint(&s).unwrap();
        assert_eq!(url, "http://localhost:8123");
        assert_eq!(user, "");
        assert_eq!(pass, "");
    }

    #[test]
    fn build_endpoint_https_scheme_defaults_to_8443() {
        let mut s = spec_with(Some("api"), Some("ch.example.com"), None, Some("analytics"));
        s.params.insert("scheme".into(), "https".into());
        let (url, user, _pass) = build_endpoint(&s).unwrap();
        assert_eq!(url, "https://ch.example.com:8443/?database=analytics");
        assert_eq!(user, "api");
    }

    #[test]
    fn build_endpoint_with_env_creds() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("MNML_DB_TEST_CH_PW", "sekret") };
        let mut s = spec_with(Some("api"), Some("ch"), Some(8123), Some("app"));
        s.creds = Some(CredsSource::Env {
            user: None,
            password: "MNML_DB_TEST_CH_PW".into(),
        });
        let (url, user, pass) = build_endpoint(&s).unwrap();
        assert_eq!(url, "http://ch:8123/?database=app");
        assert_eq!(user, "api");
        assert_eq!(pass, "sekret");
        unsafe { std::env::remove_var("MNML_DB_TEST_CH_PW") };
    }
}
