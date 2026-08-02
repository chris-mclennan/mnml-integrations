//! Redis adapter — wraps `mnml_db_driver_redis::RedisDriver` in the
//! neutral `Driver` trait.

use anyhow::{Context, Result};
use tokio::runtime::Handle;

use mnml_db_driver_redis::{
    RedisDriver, RedisEntryValue, RedisKeyKind, RedisReply, redis_commands,
};

use crate::connection::{ConnectionSpec, resolve_password};
use crate::driver::{
    Completion, CompletionCtx, CompletionKind, Driver, KeyValueEntry, KeyValueType, Namespace,
    ObjectDetail, ObjectKind, Query, QueryResult, ResultKind, SchemaObject,
};

pub struct RedisAdapter {
    inner: RedisDriver,
    description: String,
}

impl RedisAdapter {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let url = build_url(spec)?;
        let inner = RedisDriver::connect(&url).await?;
        let description = inner.describe();
        Ok(Self { inner, description })
    }
}

impl Driver for RedisAdapter {
    fn describe(&self) -> String {
        self.description.clone()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::KeyValue
    }

    fn execute(&mut self, q: &Query, row_limit: u32) -> Result<QueryResult> {
        let Query::Text(line) = q;
        let reply = Handle::current().block_on(self.inner.execute(line, row_limit))?;
        Ok(convert_reply(reply))
    }

    fn list_namespaces(&mut self) -> Result<Vec<Namespace>> {
        let out = Handle::current().block_on(self.inner.list_namespaces())?;
        Ok(out
            .into_iter()
            .map(|n| Namespace {
                name: n.name.clone(),
                label: Some(n.name),
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
                    RedisKeyKind::Stream => ObjectKind::Stream,
                    _ => ObjectKind::Key,
                },
            })
            .collect())
    }

    fn describe_object(&mut self, ns: &str, obj: &str) -> Result<ObjectDetail> {
        let d = Handle::current().block_on(self.inner.describe_object(ns, obj))?;
        let summary = d.kind.map(|k| format!("{k:?} · {}", d.peek.len()));
        Ok(ObjectDetail {
            columns: Vec::new(),
            ttl_seconds: d.ttl_seconds,
            peek: d.peek,
            summary,
        })
    }

    fn complete(&mut self, ctx: &CompletionCtx<'_>) -> Vec<Completion> {
        let prefix = ctx.current_word.to_ascii_uppercase();
        redis_commands()
            .iter()
            .filter(|c| c.starts_with(&prefix))
            .map(|c| Completion {
                insert: (*c).to_string(),
                display: (*c).to_string(),
                kind: CompletionKind::RedisCommand,
            })
            .collect()
    }
}

fn convert_reply(reply: RedisReply) -> QueryResult {
    match reply {
        RedisReply::KeyValue {
            entries,
            elapsed,
            truncated,
            server_row_count,
        } => {
            let entries: Vec<KeyValueEntry> = entries
                .into_iter()
                .map(|e| {
                    let (value, type_hint) = match &e.value {
                        RedisEntryValue::Nil => ("nil".to_string(), KeyValueType::Nil),
                        RedisEntryValue::Str(s) => (s.clone(), KeyValueType::Str),
                        RedisEntryValue::Int(n) => (n.to_string(), KeyValueType::Int),
                        RedisEntryValue::Bytes(b) => {
                            (format!("<{} bytes>", b.len()), KeyValueType::Bytes)
                        }
                    };
                    KeyValueEntry {
                        key: e.key,
                        value,
                        type_hint,
                    }
                })
                .collect();
            QueryResult::KeyValue {
                entries,
                elapsed_ms: elapsed.as_millis(),
                truncated,
                server_row_count,
            }
        }
        RedisReply::Notice { text, elapsed } => QueryResult::Notice {
            text,
            elapsed_ms: elapsed.as_millis(),
        },
    }
}

/// Build a redis:// URL from a ConnectionSpec.
fn build_url(spec: &ConnectionSpec) -> Result<String> {
    let password = resolve_password(spec).context("resolving password")?;
    let host = spec.host.as_deref().unwrap_or("localhost");
    let port = spec.port.unwrap_or(6379);
    let db = spec.database.as_deref().unwrap_or("0");
    let userinfo = if password.is_empty() {
        String::new()
    } else if let Some(user) = spec.user.as_deref() {
        format!("{user}:{password}@")
    } else {
        format!(":{password}@")
    };
    Ok(format!("redis://{userinfo}{host}:{port}/{db}"))
}
