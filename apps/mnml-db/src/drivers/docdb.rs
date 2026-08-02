//! DocDB / MongoDB adapter — wraps `mnml_db_driver_docdb::DocDbDriver`
//! in the neutral `Driver` trait.

use anyhow::{Result, anyhow};
use tokio::runtime::Handle;

use mnml_db_driver_docdb::{DocDbDriver, DocReply, mongo_keywords};

use crate::connection::ConnectionSpec;
use crate::driver::{
    Completion, CompletionCtx, CompletionKind, Driver, Namespace, ObjectDetail, ObjectKind, Query,
    QueryResult, ResultKind, SchemaObject,
};

pub struct DocDbAdapter {
    inner: DocDbDriver,
    description: String,
}

impl DocDbAdapter {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let uri = resolve_uri(spec)?;
        let inner = DocDbDriver::connect(&uri).await?;
        let description = inner.describe();
        Ok(Self { inner, description })
    }
}

impl Driver for DocDbAdapter {
    fn describe(&self) -> String {
        self.description.clone()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Document
    }

    fn execute(&mut self, q: &Query, row_limit: u32) -> Result<QueryResult> {
        let Query::Text(text) = q;
        let reply = Handle::current().block_on(self.inner.execute(text, row_limit))?;
        Ok(match reply {
            DocReply::Documents { docs, elapsed } => QueryResult::Documents {
                docs,
                elapsed_ms: elapsed.as_millis(),
            },
            DocReply::Notice { text, elapsed } => QueryResult::Notice {
                text,
                elapsed_ms: elapsed.as_millis(),
            },
        })
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
                kind: ObjectKind::Collection,
            })
            .collect())
    }

    fn describe_object(&mut self, ns: &str, obj: &str) -> Result<ObjectDetail> {
        let detail = Handle::current().block_on(self.inner.describe_object(ns, obj))?;
        let summary = Some(format!(
            "collection · {} fields (sampled {})",
            detail.fields.len(),
            detail.sample_count
        ));
        // Represent fields as ColumnDetail so the schema-detail popup
        // in v0.2 can render them uniformly with SQL columns.
        let columns = detail
            .fields
            .into_iter()
            .map(|name| crate::driver::ColumnDetail {
                name,
                data_type: String::new(),
                nullable: true,
                default: None,
            })
            .collect();
        Ok(ObjectDetail {
            columns,
            ttl_seconds: None,
            peek: Vec::new(),
            summary,
        })
    }

    fn complete(&mut self, ctx: &CompletionCtx<'_>) -> Vec<Completion> {
        let prefix = ctx.current_word;
        mongo_keywords()
            .iter()
            .filter(|k| {
                k.to_ascii_lowercase()
                    .starts_with(&prefix.to_ascii_lowercase())
            })
            .map(|k| Completion {
                insert: (*k).to_string(),
                display: (*k).to_string(),
                kind: CompletionKind::Keyword,
            })
            .collect()
    }
}

/// A DocDB / Mongo connection's creds are the URI itself. Fetch it
/// via `[connection.creds] type = "env"` if you want to keep it out
/// of the config file — otherwise `params.uri` inline (still not a
/// plaintext password field, so the validator lets it through).
fn resolve_uri(spec: &ConnectionSpec) -> Result<String> {
    // Preferred: creds.type = "env"  password = "DOCDB_URI"  — the
    // env var stores the whole mongodb URI.
    if let Some(crate::connection::CredsSource::Env { password, .. }) = &spec.creds {
        let v = std::env::var(password).map_err(|_| {
            anyhow!(
                "connection `{}`: env var `${}` is not set",
                spec.id,
                password
            )
        })?;
        return Ok(v);
    }
    // Fallback: inline `params.uri = "mongodb://..."`. This is only
    // safe when the URI doesn't embed a password — otherwise use env.
    if let Some(inline) = spec.params.get("uri") {
        return Ok(inline.clone());
    }
    Err(anyhow!(
        "connection `{}`: no docdb URI — set `[connection.creds]` type = \"env\" password = \"DOCDB_URI\", or `[connection.params]` uri = \"mongodb://...\"",
        spec.id
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::CredsSource;

    fn base(id: &str) -> ConnectionSpec {
        ConnectionSpec {
            id: id.into(),
            label: None,
            engine: "docdb".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: None,
        }
    }

    #[test]
    fn resolve_uri_prefers_env_var() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("MNML_DB_TEST_DOCDB_URI", "mongodb://alice:pw@a/b") };
        let mut s = base("d1");
        s.creds = Some(CredsSource::Env {
            user: None,
            password: "MNML_DB_TEST_DOCDB_URI".into(),
        });
        assert_eq!(resolve_uri(&s).unwrap(), "mongodb://alice:pw@a/b");
        unsafe { std::env::remove_var("MNML_DB_TEST_DOCDB_URI") };
    }

    #[test]
    fn resolve_uri_falls_back_to_inline_params() {
        let mut s = base("d2");
        s.params.insert(
            "uri".into(),
            "mongodb://cluster.example.com/app".to_string(),
        );
        assert_eq!(
            resolve_uri(&s).unwrap(),
            "mongodb://cluster.example.com/app"
        );
    }

    #[test]
    fn resolve_uri_errors_when_missing() {
        let s = base("d3");
        assert!(resolve_uri(&s).is_err());
    }

    #[test]
    fn resolve_uri_reports_missing_env_var() {
        let mut s = base("d4");
        s.creds = Some(CredsSource::Env {
            user: None,
            password: "DEFINITELY_UNSET_docdb_zzz".into(),
        });
        let err = resolve_uri(&s).unwrap_err().to_string();
        assert!(err.contains("DEFINITELY_UNSET_docdb_zzz"));
    }
}
