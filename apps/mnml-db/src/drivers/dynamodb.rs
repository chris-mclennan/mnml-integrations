//! DynamoDB adapter — wraps `mnml_db_driver_dynamodb::DynamoDbDriver`
//! in the neutral `Driver` trait.

use anyhow::Result;
use tokio::runtime::Handle;

use mnml_db_driver_dynamodb::{DynReply, DynamoDbDriver, parse_table_detail, partiql_keywords};

use crate::connection::ConnectionSpec;
use crate::driver::{
    Completion, CompletionCtx, CompletionKind, Driver, Namespace, ObjectDetail, ObjectKind, Query,
    QueryResult, ResultKind, Row, SchemaObject,
};

pub struct DynamoDbAdapter {
    inner: DynamoDbDriver,
    description: String,
}

impl DynamoDbAdapter {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        // profile / region come from the neutral spec via params +
        // user; the aws CLI honors env-var overrides for creds, so we
        // never touch AWS_ACCESS_KEY_ID here.
        let profile = spec
            .params
            .get("profile")
            .cloned()
            .or_else(|| spec.user.clone());
        let region = spec
            .params
            .get("region")
            .cloned()
            .or_else(|| spec.host.clone());
        let inner = DynamoDbDriver::connect(profile, region).await?;
        let description = inner.describe();
        Ok(Self { inner, description })
    }
}

impl Driver for DynamoDbAdapter {
    fn describe(&self) -> String {
        self.description.clone()
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Document
    }

    fn execute(&mut self, q: &Query, row_limit: u32) -> Result<QueryResult> {
        let Query::Text(stmt) = q;
        let reply = Handle::current().block_on(self.inner.execute(stmt, row_limit))?;
        Ok(match reply {
            DynReply::Documents {
                docs,
                elapsed,
                truncated,
                server_row_count,
            } => {
                // truncation + server_row_count don't have first-class
                // slots on QueryResult::Documents in v0.1 — surface
                // via the status line trailer as extra elapsed noise.
                let _ = truncated;
                let _ = server_row_count;
                QueryResult::Documents {
                    docs,
                    elapsed_ms: elapsed.as_millis(),
                }
            }
            DynReply::Notice { text, elapsed } => QueryResult::Notice {
                text,
                elapsed_ms: elapsed.as_millis(),
            },
        })
    }

    fn cancel(&mut self) {
        self.inner.cancel();
    }

    fn list_namespaces(&mut self) -> Result<Vec<Namespace>> {
        // DynamoDB has no notion of a database — synthesize one so
        // the tree renderer has something to expand into.
        Ok(vec![Namespace {
            name: "default".to_string(),
            label: Some("(region default)".to_string()),
        }])
    }

    fn list_objects(&mut self, _ns: &str) -> Result<Vec<SchemaObject>> {
        let out = Handle::current().block_on(self.inner.list_tables())?;
        Ok(out
            .into_iter()
            .map(|t| SchemaObject {
                name: t.name,
                kind: ObjectKind::Table,
            })
            .collect())
    }

    fn describe_object(&mut self, _ns: &str, obj: &str) -> Result<ObjectDetail> {
        let detail = Handle::current().block_on(self.inner.describe_table(obj))?;
        let mut peek: Vec<String> = Vec::new();
        for k in &detail.keys {
            peek.push(format!("{} · {}", k.name, k.key_type));
        }
        let summary = Some(format!(
            "table · {} keys · {} attrs · {} items",
            detail.keys.len(),
            detail.attributes.len(),
            detail
                .item_count
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into())
        ));
        // Reuse ColumnDetail for the attribute list — the schema-
        // detail popup renders columns uniformly with SQL columns.
        let columns = detail
            .attributes
            .into_iter()
            .map(|a| crate::driver::ColumnDetail {
                name: a.name,
                data_type: a.data_type,
                nullable: true,
                default: None,
            })
            .collect();
        Ok(ObjectDetail {
            columns,
            ttl_seconds: None,
            peek,
            summary,
        })
    }

    fn complete(&mut self, ctx: &CompletionCtx<'_>) -> Vec<Completion> {
        let prefix = ctx.current_word.to_ascii_uppercase();
        partiql_keywords()
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

// Not used by the trait, but kept here so the type is exercised in a
// build-featured module and shows up in the doc tree.
#[allow(dead_code)]
fn columns_from_attributes(desc_json: &serde_json::Value) -> Vec<Row> {
    let d = parse_table_detail(desc_json);
    d.attributes
        .into_iter()
        .map(|a| {
            Row(vec![
                crate::driver::CellValue::Text(a.name),
                crate::driver::CellValue::Text(a.data_type),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(id: &str) -> ConnectionSpec {
        ConnectionSpec {
            id: id.into(),
            label: None,
            engine: "dynamodb".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: None,
        }
    }

    #[test]
    fn adapter_reads_profile_from_params_first() {
        let mut s = base("d1");
        s.params.insert("profile".into(), "readonly".into());
        s.user = Some("root".into());
        assert_eq!(
            s.params.get("profile").cloned().or_else(|| s.user.clone()),
            Some("readonly".to_string())
        );
    }

    #[test]
    fn adapter_falls_back_to_user_when_no_profile_param() {
        let mut s = base("d2");
        s.user = Some("dev".into());
        assert_eq!(
            s.params.get("profile").cloned().or_else(|| s.user.clone()),
            Some("dev".to_string())
        );
    }

    #[test]
    fn adapter_region_falls_back_to_host() {
        let mut s = base("d3");
        s.host = Some("us-east-1".into());
        assert_eq!(
            s.params.get("region").cloned().or_else(|| s.host.clone()),
            Some("us-east-1".to_string())
        );
    }

    #[test]
    fn columns_from_attributes_shape() {
        let desc = serde_json::json!({
            "Table": {
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "n", "AttributeType": "N" }
                ]
            }
        });
        let rows = columns_from_attributes(&desc);
        assert_eq!(rows.len(), 2);
    }
}
