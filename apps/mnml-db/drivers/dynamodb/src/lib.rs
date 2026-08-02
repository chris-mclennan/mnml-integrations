//! DynamoDB driver crate for `mnml-db`.
//!
//! Shells out to the `aws` CLI over `tokio::process` — no
//! `aws-sdk-dynamodb` dependency, which keeps the transitive dep
//! footprint (and the release binary) small. AWS credentials come
//! from the standard chain (profile env vars, `~/.aws/credentials`,
//! IMDS, SSO). The driver simply forwards `--profile` / `--region`.
//!
//! The query surface is PartiQL — `aws dynamodb execute-statement
//! --statement "..."` — plus a handful of shell verbs (`list-tables`,
//! `describe-table`, `scan`) surfaced through the trait's schema-
//! introspection methods.
//!
//! `Notes` from the reference sibling worth preserving:
//!   - The AWS CLI accepts PartiQL as-is; multi-statement isn't
//!     supported and neither is transactions.
//!   - Pagination follows `NextToken`. To keep an accidental full
//!     table scan from hanging, we cap at 25 page fetches per query.
//!   - `describe-table` returns rich `KeySchema` — surface it in the
//!     object-detail so the user can see pk / sk fields.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// Max pages to follow via `NextToken` per query. 25 pages × 1 MB /
/// page = 25 MB scanned — enough for most exploratory queries, still
/// bounded for the runaway case.
pub const PARTIQL_MAX_PAGES: usize = 25;

pub struct DynamoDbDriver {
    profile: Option<String>,
    region: Option<String>,
    /// Cached "who am I" line for the header — `aws sts get-caller-
    /// identity`. None on offline / permission-denied.
    caller_line: Option<String>,
    /// The currently-running child pid (if any) so `cancel()` can
    /// kill it. Only one query runs per driver at a time; the shell
    /// serializes execute() calls behind the worker thread.
    running_pid: Arc<Mutex<Option<u32>>>,
}

/// A finished query — a document set. Command-shaped verbs also come
/// back as one-off documents.
#[derive(Debug, Clone)]
pub enum DynReply {
    Documents {
        docs: Vec<Value>,
        elapsed: std::time::Duration,
        truncated: bool,
        server_row_count: usize,
    },
    Notice {
        text: String,
        elapsed: std::time::Duration,
    },
}

#[derive(Debug, Clone)]
pub struct DynTable {
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct DynTableDetail {
    pub keys: Vec<DynKey>,
    pub attributes: Vec<DynAttribute>,
    pub item_count: Option<u64>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DynKey {
    pub name: String,
    /// `"HASH"` for pk, `"RANGE"` for sk.
    pub key_type: String,
}

#[derive(Debug, Clone)]
pub struct DynAttribute {
    pub name: String,
    pub data_type: String,
}

impl DynamoDbDriver {
    pub async fn connect(profile: Option<String>, region: Option<String>) -> Result<Self> {
        let mut d = Self {
            profile,
            region,
            caller_line: None,
            running_pid: Arc::new(Mutex::new(None)),
        };
        // sts get-caller-identity is a cheap unauthenticated proof
        // that credentials resolve — surface it as the describe line.
        if let Ok(Value::Object(m)) = d.run_aws(&["sts", "get-caller-identity"]).await {
            let account = m.get("Account").and_then(|v| v.as_str()).unwrap_or("?");
            let arn = m
                .get("Arn")
                .and_then(|v| v.as_str())
                .unwrap_or("(no arn)")
                .rsplit('/')
                .next()
                .unwrap_or("?");
            d.caller_line = Some(format!("{account} · {arn}"));
        }
        Ok(d)
    }

    pub fn describe(&self) -> String {
        let region = self.region.as_deref().unwrap_or("(default region)");
        match &self.caller_line {
            Some(who) => format!("DynamoDB · {who} · {region}"),
            None => format!("DynamoDB · {region}"),
        }
    }

    /// Run one PartiQL statement. The AWS CLI's PartiQL surface goes
    /// through `execute-statement`; multi-page results are followed
    /// via `NextToken` up to `PARTIQL_MAX_PAGES`.
    pub async fn execute(&mut self, statement: &str, row_limit: u32) -> Result<DynReply> {
        let start = std::time::Instant::now();
        let stmt = statement.trim().trim_end_matches(';').trim();
        if stmt.is_empty() {
            return Err(anyhow!("empty query"));
        }
        let mut all: Vec<Value> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            pages += 1;
            let mut args: Vec<String> = vec![
                "dynamodb".into(),
                "execute-statement".into(),
                "--statement".into(),
                stmt.to_string(),
            ];
            if let Some(t) = next_token.as_deref() {
                args.push("--next-token".into());
                args.push(t.to_string());
            }
            let json = self.run_aws_owned(args).await?;
            let items = json
                .get("Items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            all.extend(items);
            next_token = json
                .get("NextToken")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if all.len() >= row_limit as usize || next_token.is_none() || pages >= PARTIQL_MAX_PAGES
            {
                break;
            }
        }
        let server_row_count = all.len();
        let take = (row_limit as usize).min(all.len());
        let truncated = all.len() > take || (pages >= PARTIQL_MAX_PAGES && next_token.is_some());
        all.truncate(take);
        let docs: Vec<Value> = all.into_iter().map(flatten_attribute_value).collect();
        Ok(DynReply::Documents {
            docs,
            elapsed: start.elapsed(),
            truncated,
            server_row_count,
        })
    }

    pub async fn list_tables(&self) -> Result<Vec<DynTable>> {
        let mut out: Vec<DynTable> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut args: Vec<String> = vec!["dynamodb".into(), "list-tables".into()];
            if let Some(t) = token.as_deref() {
                args.push("--starting-token".into());
                args.push(t.to_string());
            }
            let json = self.run_aws_owned(args).await?;
            if let Some(arr) = json.get("TableNames").and_then(|v| v.as_array()) {
                for n in arr {
                    if let Some(s) = n.as_str() {
                        out.push(DynTable {
                            name: s.to_string(),
                        });
                    }
                }
            }
            token = json
                .get("NextToken")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    pub async fn describe_table(&self, table: &str) -> Result<DynTableDetail> {
        let json = self
            .run_aws(&["dynamodb", "describe-table", "--table-name", table])
            .await?;
        Ok(parse_table_detail(&json))
    }

    /// Cancel any in-flight child process spawned by this driver.
    /// Best-effort — a no-op if nothing's running.
    pub fn cancel(&mut self) {
        if let Some(pid) = self.running_pid.lock().unwrap().take() {
            #[cfg(unix)]
            unsafe {
                // SIGTERM the running aws process.
                let _ = libc_kill(pid as i32, 15);
            }
            #[cfg(not(unix))]
            {
                let _ = pid;
            }
        }
    }

    async fn run_aws(&self, args: &[&str]) -> Result<Value> {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        self.run_aws_owned(owned).await
    }

    async fn run_aws_owned(&self, args: Vec<String>) -> Result<Value> {
        let full = build_argv(self.profile.as_deref(), self.region.as_deref(), &args);
        let mut cmd = Command::new("aws");
        cmd.args(&full[1..]).kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| {
            anyhow!("spawn `aws` failed: {e} — is the AWS CLI installed and on PATH?")
        })?;
        if let Some(pid) = child.id() {
            *self.running_pid.lock().unwrap() = Some(pid);
        }
        let out = child
            .wait_with_output()
            .await
            .context("waiting for aws subprocess")?;
        *self.running_pid.lock().unwrap() = None;
        if !out.status.success() {
            return Err(anyhow!(
                "aws {} failed: {}",
                args.first().cloned().unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        if out.stdout.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&out.stdout).context("parsing aws JSON output")
    }
}

/// PartiQL keyword set used for autocomplete. Kept small and case-
/// insensitive on the caller side.
pub fn partiql_keywords() -> &'static [&'static str] {
    &[
        "SELECT",
        "UPDATE",
        "INSERT",
        "DELETE",
        "FROM",
        "WHERE",
        "AND",
        "OR",
        "NOT",
        "IN",
        "IS",
        "NULL",
        "TRUE",
        "FALSE",
        "VALUE",
        "VALUES",
        "RETURNING",
        "CONTAINS",
        "BEGINS_WITH",
    ]
}

/// Build the full argv for an `aws` call. Pulls profile / region /
/// output-json in first so callers just pass verb-scoped args.
pub fn build_argv(profile: Option<&str>, region: Option<&str>, args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec!["aws".to_string()];
    if let Some(p) = profile {
        out.push("--profile".into());
        out.push(p.to_string());
    }
    if let Some(r) = region {
        out.push("--region".into());
        out.push(r.to_string());
    }
    out.push("--output".into());
    out.push("json".into());
    out.extend(args.iter().cloned());
    out
}

/// Convert DynamoDB's `{"pk": {"S": "abc"}, "n": {"N": "42"}}` shape
/// to a flat `{"pk": "abc", "n": 42}` document. Preserves nested
/// maps / lists.
pub fn flatten_attribute_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            // Detect the AttributeValue single-key shape and unwrap.
            if map.len() == 1 {
                let (k, inner) = map.into_iter().next().unwrap();
                return decode_attr(&k, inner);
            }
            // Otherwise: walk each field & flatten.
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                out.insert(k, flatten_attribute_value(val));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(flatten_attribute_value).collect()),
        other => other,
    }
}

fn decode_attr(kind: &str, inner: Value) -> Value {
    match kind {
        "S" => inner,
        "N" => match inner.as_str() {
            Some(s) => s
                .parse::<i64>()
                .map(|n| Value::Number(n.into()))
                .or_else(|_| {
                    s.parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number)
                        .ok_or(())
                })
                .unwrap_or(Value::String(s.to_string())),
            None => inner,
        },
        "BOOL" => inner,
        "NULL" => Value::Null,
        "L" => match inner {
            Value::Array(arr) => {
                Value::Array(arr.into_iter().map(flatten_attribute_value).collect())
            }
            other => other,
        },
        "M" => flatten_attribute_value(inner),
        "SS" | "NS" | "BS" => inner,
        "B" => match inner.as_str() {
            Some(s) => Value::String(format!("<Binary: {} bytes base64>", s.len())),
            None => inner,
        },
        // Unknown or already-flattened shape — reconstruct the object.
        _ => {
            let mut m = serde_json::Map::new();
            m.insert(kind.to_string(), flatten_attribute_value(inner));
            Value::Object(m)
        }
    }
}

pub fn parse_table_detail(desc: &Value) -> DynTableDetail {
    let mut keys: Vec<DynKey> = Vec::new();
    let mut attributes: Vec<DynAttribute> = Vec::new();
    if let Some(arr) = desc.pointer("/Table/KeySchema").and_then(|v| v.as_array()) {
        for ks in arr {
            let name = ks
                .get("AttributeName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let key_type = ks
                .get("KeyType")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                keys.push(DynKey { name, key_type });
            }
        }
    }
    if let Some(arr) = desc
        .pointer("/Table/AttributeDefinitions")
        .and_then(|v| v.as_array())
    {
        for a in arr {
            let name = a
                .get("AttributeName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let data_type = a
                .get("AttributeType")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                attributes.push(DynAttribute { name, data_type });
            }
        }
    }
    let item_count = desc.pointer("/Table/ItemCount").and_then(|v| v.as_u64());
    let size_bytes = desc
        .pointer("/Table/TableSizeBytes")
        .and_then(|v| v.as_u64());
    DynTableDetail {
        keys,
        attributes,
        item_count,
        size_bytes,
    }
}

// Minimal libc kill(2) shim so we don't drag the `libc` crate in.
#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
#[cfg(unix)]
#[allow(non_snake_case)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    unsafe { kill(pid, sig) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_argv_includes_profile_and_region() {
        let got = build_argv(
            Some("dev"),
            Some("us-east-1"),
            &vec!["dynamodb".into(), "list-tables".into()],
        );
        assert_eq!(
            got,
            vec![
                "aws",
                "--profile",
                "dev",
                "--region",
                "us-east-1",
                "--output",
                "json",
                "dynamodb",
                "list-tables",
            ]
        );
    }

    #[test]
    fn build_argv_omits_missing_profile_and_region() {
        let got = build_argv(
            None,
            None,
            &vec!["sts".into(), "get-caller-identity".into()],
        );
        assert_eq!(
            got,
            vec!["aws", "--output", "json", "sts", "get-caller-identity",]
        );
    }

    #[test]
    fn flatten_string_attribute() {
        let v = serde_json::json!({ "name": { "S": "Alice" } });
        let flat = flatten_attribute_value(v);
        assert_eq!(flat, serde_json::json!({ "name": "Alice" }));
    }

    #[test]
    fn flatten_numeric_attribute_becomes_number() {
        let v = serde_json::json!({ "count": { "N": "42" } });
        let flat = flatten_attribute_value(v);
        assert_eq!(flat["count"], serde_json::json!(42));
    }

    #[test]
    fn flatten_nested_map_recurses() {
        let v = serde_json::json!({
            "profile": { "M": {
                "name": { "S": "Alice" },
                "age": { "N": "30" }
            }}
        });
        let flat = flatten_attribute_value(v);
        assert_eq!(flat["profile"]["name"], serde_json::json!("Alice"));
        assert_eq!(flat["profile"]["age"], serde_json::json!(30));
    }

    #[test]
    fn flatten_list_recurses() {
        let v = serde_json::json!({ "tags": { "L": [
            { "S": "a" },
            { "S": "b" }
        ]}});
        let flat = flatten_attribute_value(v);
        assert_eq!(flat["tags"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn flatten_binary_is_placeholder() {
        let v = serde_json::json!({ "blob": { "B": "aGVsbG8=" } });
        let flat = flatten_attribute_value(v);
        assert_eq!(flat["blob"], serde_json::json!("<Binary: 8 bytes base64>"));
    }

    #[test]
    fn flatten_null_becomes_json_null() {
        let v = serde_json::json!({ "gone": { "NULL": true } });
        let flat = flatten_attribute_value(v);
        assert_eq!(flat["gone"], serde_json::Value::Null);
    }

    #[test]
    fn parse_table_detail_extracts_keys() {
        let desc = serde_json::json!({
            "Table": {
                "KeySchema": [
                    { "AttributeName": "userId", "KeyType": "HASH" },
                    { "AttributeName": "ts", "KeyType": "RANGE" }
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "userId", "AttributeType": "S" },
                    { "AttributeName": "ts", "AttributeType": "N" }
                ],
                "ItemCount": 1234,
                "TableSizeBytes": 987654
            }
        });
        let d = parse_table_detail(&desc);
        assert_eq!(d.keys.len(), 2);
        assert_eq!(d.keys[0].name, "userId");
        assert_eq!(d.keys[0].key_type, "HASH");
        assert_eq!(d.attributes.len(), 2);
        assert_eq!(d.item_count, Some(1234));
        assert_eq!(d.size_bytes, Some(987654));
    }

    #[test]
    fn keyword_set_contains_select_and_from() {
        assert!(partiql_keywords().contains(&"SELECT"));
        assert!(partiql_keywords().contains(&"FROM"));
    }

    #[test]
    fn max_pages_is_bounded() {
        // Sanity: don't accidentally set to a huge number in a
        // refactor.
        assert!(PARTIQL_MAX_PAGES <= 100);
        assert!(PARTIQL_MAX_PAGES >= 5);
    }
}

/// Marker for deserialize-ability of AWS list-tables responses.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListTablesResp {
    #[serde(rename = "TableNames", default)]
    table_names: Vec<String>,
    #[serde(rename = "NextToken", default)]
    next_token: Option<String>,
}
