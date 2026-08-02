//! DocumentDB / MongoDB driver crate for `mnml-db`.
//!
//! Thin async wrapper around the official `mongodb` driver. Because
//! DocumentDB is MongoDB-wire-compatible (3.6 / 4.0 / 5.0), the same
//! driver serves both.
//!
//! Types stay concrete (`DocReply`, `DocNamespace`, ...) — the main
//! `mnml-db` crate owns the neutral `Driver` trait and adapts these
//! concrete types onto it. The driver crate has zero dependency on
//! the shell.
//!
//! The query surface accepts three forms:
//!   - Bare `db.<coll>.<verb>(<json>)` / `db.<coll>.<verb>()` — mongo-
//!     shell style; supported verbs are `find` and `aggregate`.
//!   - `db.getCollectionNames()` — returns collection names for the
//!     currently-active database as a one-column result.
//!   - Bare JSON `{ ... }` — interpreted as a `runCommand` payload,
//!     which lets power users hit anything the wire protocol supports.
//!
//! v0.1 renders documents as `serde_json::Value` (BSON → JSON via
//! `bson::to_bson` / serde). Binary blobs are surfaced as
//! `<Binary: N bytes>`.

use anyhow::{Context, Result, anyhow};
use bson::{Bson, Document};
use futures_util::TryStreamExt;
use mongodb::{
    Client,
    options::{AggregateOptions, FindOptions},
};

pub struct DocDbDriver {
    client: Client,
    /// Default database — pulled from the connection URI's path (or
    /// `admin` when the URI is bare).
    pub default_db: String,
    server_version: String,
    uri_summary: String,
}

/// A finished query — a rowset of documents, or a "here's a status
/// line" reply for admin commands.
#[derive(Debug, Clone)]
pub enum DocReply {
    Documents {
        docs: Vec<serde_json::Value>,
        elapsed: std::time::Duration,
    },
    Notice {
        text: String,
        elapsed: std::time::Duration,
    },
}

#[derive(Debug, Clone)]
pub struct DocNamespace {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct DocObject {
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct DocObjectDetail {
    /// Union of top-level field names discovered by sampling.
    pub fields: Vec<String>,
    pub sample_count: usize,
}

impl DocDbDriver {
    pub async fn connect(uri: &str) -> Result<Self> {
        let client = Client::with_uri_str(uri)
            .await
            .context("connecting to MongoDB / DocumentDB")?;
        let default_db = default_db_from_uri(uri);
        // buildInfo is safe on both mongo & docdb.
        let server_version = client
            .database("admin")
            .run_command(bson::doc! { "buildInfo": 1 })
            .await
            .ok()
            .and_then(|d| d.get_str("version").ok().map(str::to_string))
            .unwrap_or_else(|| "MongoDB / DocumentDB".to_string());
        Ok(Self {
            client,
            default_db,
            server_version,
            uri_summary: summarize_uri(uri),
        })
    }

    pub fn describe(&self) -> String {
        format!("MongoDB {} ({})", self.server_version, self.uri_summary)
    }

    pub async fn execute(&self, raw: &str, row_limit: u32) -> Result<DocReply> {
        let start = std::time::Instant::now();
        let parsed = parse_query(raw)?;
        let docs = self
            .dispatch(parsed, row_limit)
            .await
            .context("running mongo query")?;
        Ok(DocReply::Documents {
            docs,
            elapsed: start.elapsed(),
        })
    }

    async fn dispatch(&self, q: ParsedQuery, row_limit: u32) -> Result<Vec<serde_json::Value>> {
        match q {
            ParsedQuery::Find {
                db,
                coll,
                filter,
                projection,
            } => {
                let db_name = db.unwrap_or_else(|| self.default_db.clone());
                let coll = self.client.database(&db_name).collection::<Document>(&coll);
                let opts = match projection {
                    Some(p) => FindOptions::builder()
                        .limit(Some(row_limit as i64))
                        .projection(p)
                        .build(),
                    None => FindOptions::builder().limit(Some(row_limit as i64)).build(),
                };
                let mut cursor = coll
                    .find(filter)
                    .with_options(opts)
                    .await
                    .context("running find()")?;
                let mut out = Vec::new();
                while let Some(d) = cursor.try_next().await.context("draining cursor")? {
                    out.push(doc_to_json(&d));
                    if out.len() >= row_limit as usize {
                        break;
                    }
                }
                Ok(out)
            }
            ParsedQuery::Aggregate { db, coll, pipeline } => {
                let db_name = db.unwrap_or_else(|| self.default_db.clone());
                let coll = self.client.database(&db_name).collection::<Document>(&coll);
                let opts = AggregateOptions::default();
                let mut cursor = coll
                    .aggregate(pipeline)
                    .with_options(opts)
                    .await
                    .context("running aggregate()")?;
                let mut out = Vec::new();
                while let Some(d) = cursor.try_next().await.context("draining cursor")? {
                    out.push(doc_to_json(&d));
                    if out.len() >= row_limit as usize {
                        break;
                    }
                }
                Ok(out)
            }
            ParsedQuery::GetCollectionNames { db } => {
                let db_name = db.unwrap_or_else(|| self.default_db.clone());
                let names = self
                    .client
                    .database(&db_name)
                    .list_collection_names()
                    .await
                    .context("listing collections")?;
                Ok(names
                    .into_iter()
                    .map(|n| serde_json::json!({ "collection": n }))
                    .collect())
            }
            ParsedQuery::RunCommand { db, cmd } => {
                let db_name = db.unwrap_or_else(|| self.default_db.clone());
                let reply = self
                    .client
                    .database(&db_name)
                    .run_command(cmd)
                    .await
                    .context("runCommand")?;
                Ok(vec![doc_to_json(&reply)])
            }
        }
    }

    pub async fn list_namespaces(&self) -> Result<Vec<DocNamespace>> {
        let names = self
            .client
            .list_database_names()
            .await
            .context("listing databases")?;
        Ok(names
            .into_iter()
            .map(|name| DocNamespace { name })
            .collect())
    }

    pub async fn list_objects(&self, ns: &str) -> Result<Vec<DocObject>> {
        let names = self
            .client
            .database(ns)
            .list_collection_names()
            .await
            .context("listing collections")?;
        Ok(names.into_iter().map(|name| DocObject { name }).collect())
    }

    /// Sample up to `sample` documents from a collection and return
    /// the union of their top-level field names.
    pub async fn describe_object(&self, ns: &str, coll: &str) -> Result<DocObjectDetail> {
        let coll_h = self.client.database(ns).collection::<Document>(coll);
        let opts = FindOptions::builder().limit(Some(5)).build();
        let mut cursor = coll_h
            .find(bson::doc! {})
            .with_options(opts)
            .await
            .context("sampling collection")?;
        let mut fields: std::collections::BTreeSet<String> = Default::default();
        let mut sample = 0usize;
        while let Some(d) = cursor.try_next().await.context("draining cursor")? {
            sample += 1;
            for k in d.keys() {
                fields.insert(k.to_string());
            }
        }
        Ok(DocObjectDetail {
            fields: fields.into_iter().collect(),
            sample_count: sample,
        })
    }
}

/// Convert a BSON document to a serde_json Value, with a friendly
/// placeholder for binary blobs (which serialize badly otherwise).
pub fn doc_to_json(d: &Document) -> serde_json::Value {
    let bson = Bson::Document(d.clone());
    bson_to_json(&bson)
}

fn bson_to_json(b: &Bson) -> serde_json::Value {
    match b {
        Bson::Binary(bin) => {
            serde_json::Value::String(format!("<Binary: {} bytes>", bin.bytes.len()))
        }
        Bson::Document(d) => {
            let mut map = serde_json::Map::new();
            for (k, v) in d {
                map.insert(k.clone(), bson_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        Bson::Array(arr) => serde_json::Value::Array(arr.iter().map(bson_to_json).collect()),
        // Everything else goes through serde's default rep; that turns
        // ObjectId → { "$oid": "..." }, Timestamp → { "$timestamp":
        // {...}}, etc. Callers who want a flatter shape can post-
        // process.
        other => serde_json::to_value(other).unwrap_or(serde_json::Value::Null),
    }
}

#[derive(Debug, Clone)]
pub enum ParsedQuery {
    Find {
        db: Option<String>,
        coll: String,
        filter: Document,
        projection: Option<Document>,
    },
    Aggregate {
        db: Option<String>,
        coll: String,
        pipeline: Vec<Document>,
    },
    GetCollectionNames {
        db: Option<String>,
    },
    RunCommand {
        db: Option<String>,
        cmd: Document,
    },
}

/// Parse mongo-shell-ish inputs:
///   - `db.<coll>.find(<json>)` / `db.<coll>.find(<json>, <proj>)`
///   - `db.<coll>.aggregate([<pipeline>])`
///   - `db.getCollectionNames()`
///   - Bare JSON `{ ... }` — treated as `runCommand`.
pub fn parse_query(input: &str) -> Result<ParsedQuery> {
    let trimmed = input.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty query"));
    }
    if trimmed.starts_with('{') {
        let cmd = parse_json_doc(trimmed)?;
        return Ok(ParsedQuery::RunCommand { db: None, cmd });
    }
    // Shell-style dispatch.
    let open = trimmed
        .find('(')
        .ok_or_else(|| anyhow!("expected `db.<coll>.find(...)` or JSON `{{...}}`"))?;
    let close = trimmed
        .rfind(')')
        .ok_or_else(|| anyhow!("missing closing `)`"))?;
    if close < open {
        return Err(anyhow!("mismatched parens"));
    }
    let prefix = &trimmed[..open];
    let body = trimmed[open + 1..close].trim();
    let segments: Vec<&str> = prefix.split('.').collect();

    // db.getCollectionNames()
    if segments.len() == 2
        && segments[0].eq_ignore_ascii_case("db")
        && segments[1] == "getCollectionNames"
    {
        return Ok(ParsedQuery::GetCollectionNames { db: None });
    }

    // Split off the verb (last segment) — everything before is db /
    // collection identifiers.
    let (verb, path) = segments
        .split_last()
        .ok_or_else(|| anyhow!("bad query shape"))?;
    let (db, coll) = match path {
        // `<coll>.<verb>(...)` — `db` is left as literal `db` in mongo
        // shell; treat as "no db specified".
        [only] => {
            if only.eq_ignore_ascii_case("db") {
                return Err(anyhow!("missing collection name"));
            }
            (None, (*only).to_string())
        }
        // `db.<coll>.<verb>(...)`
        [first, coll] if first.eq_ignore_ascii_case("db") => (None, (*coll).to_string()),
        // `<db>.<coll>.<verb>(...)`
        [db, coll] => (Some((*db).to_string()), (*coll).to_string()),
        // `db.<db2>.<coll>.<verb>(...)` — support the redundant form.
        [first, db, coll] if first.eq_ignore_ascii_case("db") => {
            (Some((*db).to_string()), (*coll).to_string())
        }
        _ => return Err(anyhow!("unrecognised query prefix `{prefix}`")),
    };

    match *verb {
        "find" => {
            let (filter_txt, proj_txt) = split_top_level_comma(body);
            let filter = if filter_txt.is_empty() {
                Document::new()
            } else {
                parse_json_doc(filter_txt)?
            };
            let projection = match proj_txt {
                None | Some("") => None,
                Some(p) => Some(parse_json_doc(p)?),
            };
            Ok(ParsedQuery::Find {
                db,
                coll,
                filter,
                projection,
            })
        }
        "aggregate" => {
            let body = if body.is_empty() { "[]" } else { body };
            let pipeline = parse_json_array(body)?;
            Ok(ParsedQuery::Aggregate { db, coll, pipeline })
        }
        other => Err(anyhow!(
            "unsupported verb `{other}` — v0.1 supports `find` / `aggregate` / `db.getCollectionNames()`. Wrap arbitrary commands in a JSON `{{ \"cmd\": 1 }}` payload to route through `runCommand`."
        )),
    }
}

/// Split a call body on the first top-level comma. Ignores commas
/// nested inside `{}` / `[]` / quoted strings.
fn split_top_level_comma(s: &str) -> (&str, Option<&str>) {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                return (s[..i].trim(), Some(s[i + 1..].trim()));
            }
            _ => {}
        }
    }
    (s.trim(), None)
}

fn parse_json_doc(s: &str) -> Result<Document> {
    let v: serde_json::Value =
        serde_json::from_str(s).with_context(|| format!("parsing JSON object: {s}"))?;
    let bson: Bson = bson::to_bson(&v).context("converting JSON → BSON")?;
    match bson {
        Bson::Document(d) => Ok(d),
        _ => Err(anyhow!("expected a JSON object, got: {v}")),
    }
}

fn parse_json_array(s: &str) -> Result<Vec<Document>> {
    let v: serde_json::Value =
        serde_json::from_str(s).with_context(|| format!("parsing JSON array: {s}"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("aggregate pipeline must be a JSON array"))?;
    arr.iter()
        .map(|el| {
            let b: Bson = bson::to_bson(el).context("converting pipeline stage → BSON")?;
            match b {
                Bson::Document(d) => Ok(d),
                _ => Err(anyhow!("pipeline stages must be JSON objects, got: {el}")),
            }
        })
        .collect()
}

/// The mongo-shell verb keyword set used for autocomplete.
pub fn mongo_keywords() -> &'static [&'static str] {
    &[
        "find",
        "aggregate",
        "insertOne",
        "insertMany",
        "updateOne",
        "updateMany",
        "deleteOne",
        "deleteMany",
        "countDocuments",
        "distinct",
        "getCollectionNames",
        "listDatabases",
        "listCollections",
        "runCommand",
    ]
}

/// Extract the default database from a mongodb URI. The path after
/// the host / port block is the database name; falls back to `admin`.
pub fn default_db_from_uri(uri: &str) -> String {
    let Some(scheme_end) = uri.find("://") else {
        return "admin".to_string();
    };
    let rest = &uri[scheme_end + 3..];
    let after_at = match rest.find('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    let after_slash = match after_at.find('/') {
        Some(sl) => &after_at[sl + 1..],
        None => return "admin".to_string(),
    };
    let path = after_slash.split('?').next().unwrap_or("");
    if path.is_empty() {
        "admin".to_string()
    } else {
        path.to_string()
    }
}

/// Trim a mongodb URI down to a short "host[:port]/db" label suitable
/// for a header chip.
pub fn summarize_uri(uri: &str) -> String {
    let Some(scheme_end) = uri.find("://") else {
        return uri.to_string();
    };
    let rest = &uri[scheme_end + 3..];
    let after_at = match rest.find('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    after_at.split('?').next().unwrap_or(after_at).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_find_bare_collection() {
        let p = parse_query("users.find({})").unwrap();
        match p {
            ParsedQuery::Find {
                db,
                coll,
                filter,
                projection,
            } => {
                assert!(db.is_none());
                assert_eq!(coll, "users");
                assert!(filter.is_empty());
                assert!(projection.is_none());
            }
            _ => panic!("expected Find"),
        }
    }

    #[test]
    fn parse_find_with_db_and_projection() {
        let p =
            parse_query(r#"analytics.events.find({"type":"click"}, {"_id":0,"ts":1})"#).unwrap();
        match p {
            ParsedQuery::Find {
                db,
                coll,
                filter,
                projection,
            } => {
                assert_eq!(db.as_deref(), Some("analytics"));
                assert_eq!(coll, "events");
                assert_eq!(filter.get_str("type").unwrap(), "click");
                let proj = projection.expect("projection populated");
                // bson::to_bson turns JSON numbers into i64.
                assert_eq!(proj.get_i64("_id").ok(), Some(0));
                assert_eq!(proj.get_i64("ts").ok(), Some(1));
            }
            _ => panic!("expected Find"),
        }
    }

    #[test]
    fn parse_find_via_db_prefix() {
        let p = parse_query("db.users.find({})").unwrap();
        match p {
            ParsedQuery::Find { db, coll, .. } => {
                assert!(db.is_none());
                assert_eq!(coll, "users");
            }
            _ => panic!("expected Find"),
        }
    }

    #[test]
    fn parse_aggregate_with_pipeline() {
        let p = parse_query(r#"orders.aggregate([{"$match":{"status":"paid"}},{"$count":"n"}])"#)
            .unwrap();
        match p {
            ParsedQuery::Aggregate { db, coll, pipeline } => {
                assert!(db.is_none());
                assert_eq!(coll, "orders");
                assert_eq!(pipeline.len(), 2);
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_get_collection_names() {
        let p = parse_query("db.getCollectionNames()").unwrap();
        assert!(matches!(p, ParsedQuery::GetCollectionNames { db: None }));
    }

    #[test]
    fn parse_bare_json_is_run_command() {
        let p = parse_query(r#"{"buildInfo":1}"#).unwrap();
        match p {
            ParsedQuery::RunCommand { db, cmd } => {
                assert!(db.is_none());
                assert_eq!(cmd.get_i64("buildInfo").ok(), Some(1));
            }
            _ => panic!("expected RunCommand"),
        }
    }

    #[test]
    fn parse_rejects_unknown_verb() {
        assert!(parse_query("users.insertOne({\"a\":1})").is_err());
    }

    #[test]
    fn parse_rejects_missing_paren() {
        assert!(parse_query("users.find").is_err());
    }

    #[test]
    fn parse_trims_trailing_semicolon() {
        let p = parse_query("users.find({});").unwrap();
        assert!(matches!(p, ParsedQuery::Find { .. }));
    }

    #[test]
    fn split_top_level_comma_ignores_nested() {
        let (a, b) = split_top_level_comma(r#"{"a":{"b":1,"c":2}}, {"_id":0}"#);
        assert_eq!(a, r#"{"a":{"b":1,"c":2}}"#);
        assert_eq!(b, Some(r#"{"_id":0}"#));
    }

    #[test]
    fn split_top_level_comma_none_when_single() {
        let (a, b) = split_top_level_comma("{}");
        assert_eq!(a, "{}");
        assert!(b.is_none());
    }

    #[test]
    fn summarize_uri_strips_userinfo_and_query() {
        assert_eq!(
            summarize_uri("mongodb://alice:pw@cluster.example.com:27017/app?replicaSet=rs0"),
            "cluster.example.com:27017/app"
        );
        assert_eq!(
            summarize_uri("mongodb+srv://cluster.example.com/app"),
            "cluster.example.com/app"
        );
    }

    #[test]
    fn default_db_from_uri_reads_path() {
        assert_eq!(
            default_db_from_uri("mongodb://alice:pw@cluster.example.com:27017/app"),
            "app"
        );
        assert_eq!(
            default_db_from_uri("mongodb://cluster.example.com:27017"),
            "admin"
        );
        assert_eq!(
            default_db_from_uri("mongodb://cluster.example.com:27017/"),
            "admin"
        );
        assert_eq!(
            default_db_from_uri("mongodb://cluster.example.com:27017/app?replicaSet=rs0"),
            "app"
        );
    }

    #[test]
    fn doc_to_json_placeholder_for_binary() {
        let mut d = Document::new();
        d.insert(
            "blob",
            Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: vec![0u8; 42],
            }),
        );
        let v = doc_to_json(&d);
        assert_eq!(v["blob"], serde_json::json!("<Binary: 42 bytes>"));
    }

    #[test]
    fn keyword_set_contains_find_and_aggregate() {
        assert!(mongo_keywords().contains(&"find"));
        assert!(mongo_keywords().contains(&"aggregate"));
    }
}
