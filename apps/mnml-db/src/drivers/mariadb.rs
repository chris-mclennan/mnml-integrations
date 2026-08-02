//! MariaDB adapter — wraps `mnml_db_driver_mariadb::MariaDriver`
//! in the neutral `Driver` trait.

use anyhow::{Context, Result};
use tokio::runtime::Handle;

use mnml_db_driver_mariadb::{MariaDriver, MyCell, MyObjectKind, MyReply, sql_keywords};

use crate::connection::{ConnectionSpec, resolve_password};
use crate::driver::{
    CellValue, Column, Completion, CompletionCtx, CompletionKind, Driver, Namespace, ObjectDetail,
    ObjectKind, Query, QueryResult, ResultKind, Row, SchemaObject,
};

pub struct MariaAdapter {
    inner: MariaDriver,
    description: String,
}

impl MariaAdapter {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let dsn = build_dsn(spec)?;
        let inner = MariaDriver::connect(&dsn).await?;
        let description = inner.describe();
        Ok(Self { inner, description })
    }
}

impl Driver for MariaAdapter {
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
                    MyObjectKind::Table => ObjectKind::Table,
                    MyObjectKind::View => ObjectKind::View,
                    MyObjectKind::Sequence => ObjectKind::Sequence,
                    MyObjectKind::Other => ObjectKind::Other,
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

fn convert_reply(reply: MyReply) -> QueryResult {
    match reply {
        MyReply::Rows {
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
                            MyCell::Null => CellValue::Null,
                            MyCell::Text(s) => CellValue::Text(s),
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
        MyReply::Notice { tag, elapsed } => QueryResult::Notice {
            text: tag,
            elapsed_ms: elapsed.as_millis(),
        },
    }
}

/// Build a MySQL / MariaDB DSN from the neutral ConnectionSpec —
/// resolves the credential source into inline `user:pass` in the URL.
fn build_dsn(spec: &ConnectionSpec) -> Result<String> {
    let password = resolve_password(spec).context("resolving password")?;
    let user = spec.user.as_deref().unwrap_or("root");
    let host = spec.host.as_deref().unwrap_or("localhost");
    let port = spec.port.unwrap_or(3306);
    let db = spec.database.as_deref().unwrap_or("");
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
    let path = if db.is_empty() {
        String::new()
    } else {
        format!("/{db}")
    };
    Ok(format!("mysql://{userinfo}@{host}:{port}{path}"))
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
            engine: "mariadb".into(),
            host: host.map(str::to_string),
            port,
            user: user.map(str::to_string),
            database: db.map(str::to_string),
            params: Default::default(),
            creds: None,
        }
    }

    #[test]
    fn build_dsn_no_creds_no_db() {
        let s = spec_with(None, None, None, None);
        assert_eq!(build_dsn(&s).unwrap(), "mysql://root@localhost:3306");
    }

    #[test]
    fn build_dsn_with_env_creds_and_db() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("MNML_DB_TEST_MYSQL_PW", "sekret") };
        let mut s = spec_with(Some("api"), Some("db.example.com"), Some(3307), Some("app"));
        s.creds = Some(CredsSource::Env {
            user: None,
            password: "MNML_DB_TEST_MYSQL_PW".into(),
        });
        let dsn = build_dsn(&s).unwrap();
        assert_eq!(dsn, "mysql://api:sekret@db.example.com:3307/app");
        unsafe { std::env::remove_var("MNML_DB_TEST_MYSQL_PW") };
    }

    #[test]
    fn build_dsn_percent_encodes_special_chars_in_password() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("MNML_DB_TEST_MYSQL_PW2", "p@ss:word") };
        let mut s = spec_with(Some("root"), None, None, None);
        s.creds = Some(CredsSource::Env {
            user: None,
            password: "MNML_DB_TEST_MYSQL_PW2".into(),
        });
        let dsn = build_dsn(&s).unwrap();
        assert_eq!(dsn, "mysql://root:p%40ss%3Aword@localhost:3306");
        unsafe { std::env::remove_var("MNML_DB_TEST_MYSQL_PW2") };
    }
}
