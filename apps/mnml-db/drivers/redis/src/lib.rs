//! Redis driver crate for `mnml-db`.
//!
//! Wraps the `redis` crate's `ConnectionManager` (auto-reconnects
//! under the hood) and exposes the shape the shell needs: a command
//! executor that tokenizes an input line into argv (respecting
//! quotes), plus schema-introspection helpers.
//!
//! Types stay concrete (`RedisReply`, `RedisEntry`, ...) — the main
//! `mnml-db` crate owns the neutral `Driver` trait and adapts these
//! onto it. Zero dependency on the shell.

use anyhow::{Context, Result};
use redis::{Value, aio::ConnectionManager};

/// Live Redis connection + a cached server-version string.
pub struct RedisDriver {
    conn: ConnectionManager,
    server_version: String,
    url_summary: String,
}

/// A finished command — either a KeyValue-shaped reply, or a
/// scalar/notice ("OK", "PONG", integers).
#[derive(Debug, Clone)]
pub enum RedisReply {
    KeyValue {
        entries: Vec<RedisEntry>,
        elapsed: std::time::Duration,
        truncated: bool,
        server_row_count: usize,
    },
    Notice {
        text: String,
        elapsed: std::time::Duration,
    },
}

/// One row in a KeyValue result. `value` mirrors the shape of what
/// Redis returned so the UI can decide how to render it.
#[derive(Debug, Clone)]
pub struct RedisEntry {
    pub key: String,
    pub value: RedisEntryValue,
}

#[derive(Debug, Clone)]
pub enum RedisEntryValue {
    Nil,
    Str(String),
    Int(i64),
    Bytes(Vec<u8>),
}

impl RedisEntryValue {
    pub fn as_display(&self) -> String {
        match self {
            RedisEntryValue::Nil => "nil".to_string(),
            RedisEntryValue::Str(s) => s.clone(),
            RedisEntryValue::Int(n) => n.to_string(),
            RedisEntryValue::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        }
    }
}

/// A "namespace" — for Redis, a database index (0..15 by default).
#[derive(Debug, Clone)]
pub struct RedisNamespace {
    pub name: String,
}

/// A key visible under a namespace. `kind` mirrors `TYPE key`.
#[derive(Debug, Clone)]
pub struct RedisObject {
    pub name: String,
    pub kind: RedisKeyKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisKeyKind {
    String,
    List,
    Set,
    Hash,
    ZSet,
    Stream,
    Unknown,
}

/// Detail for `describe_object` — TTL + peek at the first N values.
#[derive(Debug, Clone, Default)]
pub struct RedisObjectDetail {
    pub kind: Option<RedisKeyKind>,
    pub ttl_seconds: Option<i64>,
    pub peek: Vec<String>,
}

impl RedisDriver {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).context("parsing Redis URL")?;
        let mut conn = ConnectionManager::new(client)
            .await
            .context("connecting to Redis")?;
        let server_version = redis::cmd("INFO")
            .arg("server")
            .query_async::<Value>(&mut conn)
            .await
            .ok()
            .and_then(|v| match v {
                Value::BulkString(bytes) => {
                    let s = String::from_utf8_lossy(&bytes);
                    s.lines()
                        .find_map(|l| l.strip_prefix("redis_version:"))
                        .map(|s| s.trim().to_string())
                }
                Value::SimpleString(s) => Some(s),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_string());
        let url_summary = summarize_url(url);
        Ok(Self {
            conn,
            server_version,
            url_summary,
        })
    }

    pub fn describe(&self) -> String {
        format!("Redis {} ({})", self.server_version, self.url_summary)
    }

    /// Run a raw Redis command line. Tokenizes via `tokenize` (quote-
    /// aware) so `HSET user:1 name "alice jones"` works.
    pub async fn execute(&mut self, line: &str, row_limit: u32) -> Result<RedisReply> {
        let argv = tokenize(line)?;
        if argv.is_empty() {
            anyhow::bail!("empty command");
        }
        let mut cmd = redis::cmd(&argv[0]);
        for a in &argv[1..] {
            cmd.arg(a.as_bytes());
        }
        let start = std::time::Instant::now();
        let value: Value = cmd
            .query_async(&mut self.conn)
            .await
            .with_context(|| format!("running `{line}`"))?;
        let elapsed = start.elapsed();

        // Interpret the value in the context of the verb — HGETALL
        // returns a flat array of alternating field/value, whereas
        // KEYS returns a flat array of names.
        let verb = argv[0].to_ascii_uppercase();
        interpret(&verb, value, elapsed, row_limit)
    }

    /// Redis has no schema; the closest concept is the numbered
    /// database index. v0.1 exposes `db0`..`db15`, which is the
    /// out-of-the-box config for a standalone Redis instance.
    pub async fn list_namespaces(&self) -> Result<Vec<RedisNamespace>> {
        Ok((0..16)
            .map(|i| RedisNamespace {
                name: format!("db{i}"),
            })
            .collect())
    }

    /// Best-effort SCAN — walks the current DB once with a bounded
    /// COUNT. Not resumable in v0.1; keys past the first ~200 don't
    /// appear until you re-SCAN.
    pub async fn list_objects(&mut self, _ns: &str) -> Result<Vec<RedisObject>> {
        // SCAN cursor 0 COUNT 200
        let value: Value = redis::cmd("SCAN")
            .arg(0)
            .arg("COUNT")
            .arg(200)
            .query_async(&mut self.conn)
            .await
            .context("SCAN")?;
        let names = extract_scan_names(&value);
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let kind = self.key_type(&name).await.unwrap_or(RedisKeyKind::Unknown);
            out.push(RedisObject { name, kind });
        }
        Ok(out)
    }

    pub async fn describe_object(&mut self, _ns: &str, key: &str) -> Result<RedisObjectDetail> {
        let kind = self.key_type(key).await.ok();
        let ttl: Option<i64> = redis::cmd("TTL")
            .arg(key)
            .query_async(&mut self.conn)
            .await
            .ok();
        let peek = self.peek(key, kind).await.unwrap_or_default();
        Ok(RedisObjectDetail {
            kind,
            ttl_seconds: ttl,
            peek,
        })
    }

    async fn key_type(&mut self, key: &str) -> Result<RedisKeyKind> {
        let value: Value = redis::cmd("TYPE")
            .arg(key)
            .query_async(&mut self.conn)
            .await?;
        let s = match value {
            Value::SimpleString(s) => s,
            Value::BulkString(b) => String::from_utf8_lossy(&b).to_string(),
            _ => "none".to_string(),
        };
        Ok(match s.as_str() {
            "string" => RedisKeyKind::String,
            "list" => RedisKeyKind::List,
            "set" => RedisKeyKind::Set,
            "hash" => RedisKeyKind::Hash,
            "zset" => RedisKeyKind::ZSet,
            "stream" => RedisKeyKind::Stream,
            _ => RedisKeyKind::Unknown,
        })
    }

    async fn peek(&mut self, key: &str, kind: Option<RedisKeyKind>) -> Result<Vec<String>> {
        let value: Value = match kind {
            Some(RedisKeyKind::String) => {
                redis::cmd("GET")
                    .arg(key)
                    .query_async(&mut self.conn)
                    .await?
            }
            Some(RedisKeyKind::List) => {
                redis::cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(19)
                    .query_async(&mut self.conn)
                    .await?
            }
            Some(RedisKeyKind::Hash) => {
                redis::cmd("HGETALL")
                    .arg(key)
                    .query_async(&mut self.conn)
                    .await?
            }
            Some(RedisKeyKind::Set) => {
                redis::cmd("SMEMBERS")
                    .arg(key)
                    .query_async(&mut self.conn)
                    .await?
            }
            Some(RedisKeyKind::ZSet) => {
                redis::cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(19)
                    .arg("WITHSCORES")
                    .query_async(&mut self.conn)
                    .await?
            }
            _ => return Ok(Vec::new()),
        };
        Ok(flatten_scalar(&value))
    }
}

/// Redis has a fixed command surface. Kept small in v0.1 — the
/// ~40 commands below cover most day-to-day exploration; the full
/// ~200 can be added later.
pub fn redis_commands() -> &'static [&'static str] {
    &[
        "GET",
        "SET",
        "DEL",
        "EXISTS",
        "EXPIRE",
        "TTL",
        "PERSIST",
        "TYPE",
        "KEYS",
        "SCAN",
        "INFO",
        "CONFIG",
        "PING",
        "DBSIZE",
        "FLUSHDB",
        "SELECT",
        "INCR",
        "DECR",
        "MGET",
        "MSET",
        "APPEND",
        "STRLEN",
        "HGET",
        "HSET",
        "HGETALL",
        "HKEYS",
        "HVALS",
        "HDEL",
        "HEXISTS",
        "LPUSH",
        "RPUSH",
        "LPOP",
        "RPOP",
        "LRANGE",
        "LLEN",
        "SADD",
        "SMEMBERS",
        "SISMEMBER",
        "SCARD",
        "SREM",
        "ZADD",
        "ZRANGE",
        "ZREM",
        "ZSCORE",
        "ZCARD",
        "XADD",
        "XRANGE",
        "XLEN",
    ]
}

/// Split an input line into argv, respecting double- and single-
/// quoted spans and `\\`-escaped characters. Returns an error on an
/// unterminated quote so the shell can surface a clear message.
pub fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_double || in_single => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_single || in_double {
        anyhow::bail!("unterminated quote");
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn interpret(
    verb: &str,
    value: Value,
    elapsed: std::time::Duration,
    row_limit: u32,
) -> Result<RedisReply> {
    match verb {
        "HGETALL" | "CONFIG" => Ok(pairs_from_flat_array(value, elapsed, row_limit)),
        "SET" | "PING" | "DEL" | "EXPIRE" | "EXISTS" | "PERSIST" | "SELECT" | "FLUSHDB"
        | "AUTH" => Ok(RedisReply::Notice {
            text: scalar_to_string(&value),
            elapsed,
        }),
        _ => Ok(default_interpret(value, elapsed, row_limit)),
    }
}

fn default_interpret(value: Value, elapsed: std::time::Duration, row_limit: u32) -> RedisReply {
    match &value {
        Value::Nil | Value::Int(_) | Value::SimpleString(_) | Value::Okay => RedisReply::Notice {
            text: scalar_to_string(&value),
            elapsed,
        },
        Value::BulkString(bytes) => RedisReply::KeyValue {
            entries: vec![RedisEntry {
                key: "value".to_string(),
                value: RedisEntryValue::Str(String::from_utf8_lossy(bytes).to_string()),
            }],
            elapsed,
            truncated: false,
            server_row_count: 1,
        },
        Value::Array(items) | Value::Set(items) => {
            let all_scalar = items.iter().all(is_scalar);
            if all_scalar && !items.is_empty() && items.len() % 2 == 0 && looks_like_pairs(items) {
                let entries: Vec<RedisEntry> = items
                    .chunks(2)
                    .map(|pair| RedisEntry {
                        key: scalar_to_string(&pair[0]),
                        value: value_to_entry_value(&pair[1]),
                    })
                    .collect();
                let server_row_count = entries.len();
                let truncated = server_row_count > row_limit as usize;
                let entries = entries.into_iter().take(row_limit as usize).collect();
                RedisReply::KeyValue {
                    entries,
                    elapsed,
                    truncated,
                    server_row_count,
                }
            } else {
                let entries: Vec<RedisEntry> = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| RedisEntry {
                        key: i.to_string(),
                        value: value_to_entry_value(v),
                    })
                    .collect();
                let server_row_count = entries.len();
                let truncated = server_row_count > row_limit as usize;
                let entries = entries.into_iter().take(row_limit as usize).collect();
                RedisReply::KeyValue {
                    entries,
                    elapsed,
                    truncated,
                    server_row_count,
                }
            }
        }
        Value::Map(pairs) => {
            let entries: Vec<RedisEntry> = pairs
                .iter()
                .map(|(k, v)| RedisEntry {
                    key: scalar_to_string(k),
                    value: value_to_entry_value(v),
                })
                .collect();
            let server_row_count = entries.len();
            let truncated = server_row_count > row_limit as usize;
            let entries = entries.into_iter().take(row_limit as usize).collect();
            RedisReply::KeyValue {
                entries,
                elapsed,
                truncated,
                server_row_count,
            }
        }
        _ => RedisReply::Notice {
            text: format!("{value:?}"),
            elapsed,
        },
    }
}

fn pairs_from_flat_array(value: Value, elapsed: std::time::Duration, row_limit: u32) -> RedisReply {
    let items = match value {
        Value::Array(items) | Value::Set(items) => items,
        Value::Map(pairs) => {
            let entries: Vec<RedisEntry> = pairs
                .iter()
                .map(|(k, v)| RedisEntry {
                    key: scalar_to_string(k),
                    value: value_to_entry_value(v),
                })
                .collect();
            let server_row_count = entries.len();
            let truncated = server_row_count > row_limit as usize;
            let entries = entries.into_iter().take(row_limit as usize).collect();
            return RedisReply::KeyValue {
                entries,
                elapsed,
                truncated,
                server_row_count,
            };
        }
        other => {
            return RedisReply::Notice {
                text: format!("{other:?}"),
                elapsed,
            };
        }
    };
    let entries: Vec<RedisEntry> = items
        .chunks(2)
        .filter_map(|pair| {
            if pair.len() == 2 {
                Some(RedisEntry {
                    key: scalar_to_string(&pair[0]),
                    value: value_to_entry_value(&pair[1]),
                })
            } else {
                None
            }
        })
        .collect();
    let server_row_count = entries.len();
    let truncated = server_row_count > row_limit as usize;
    let entries = entries.into_iter().take(row_limit as usize).collect();
    RedisReply::KeyValue {
        entries,
        elapsed,
        truncated,
        server_row_count,
    }
}

fn extract_scan_names(v: &Value) -> Vec<String> {
    // SCAN returns [cursor, [key, key, ...]].
    if let Value::Array(items) = v
        && items.len() == 2
        && let Value::Array(keys) = &items[1]
    {
        return keys.iter().map(scalar_to_string).collect();
    }
    Vec::new()
}

fn flatten_scalar(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) | Value::Set(items) => items.iter().map(scalar_to_string).collect(),
        Value::Nil => Vec::new(),
        _ => vec![scalar_to_string(v)],
    }
}

fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Nil | Value::Int(_) | Value::BulkString(_) | Value::SimpleString(_) | Value::Okay
    )
}

/// Heuristic: even-length flat scalar array is "probably pairs" when
/// the odd-indexed elements look like they could be values (short
/// bulk strings, ints). Real fix: use the verb — most callers do.
fn looks_like_pairs(items: &[Value]) -> bool {
    items.len() >= 2 && items.len().is_multiple_of(2)
}

fn scalar_to_string(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Int(n) => n.to_string(),
        Value::BulkString(b) => String::from_utf8_lossy(b).to_string(),
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".to_string(),
        other => format!("{other:?}"),
    }
}

fn value_to_entry_value(v: &Value) -> RedisEntryValue {
    match v {
        Value::Nil => RedisEntryValue::Nil,
        Value::Int(n) => RedisEntryValue::Int(*n),
        Value::BulkString(b) => match std::str::from_utf8(b) {
            Ok(s) => RedisEntryValue::Str(s.to_string()),
            Err(_) => RedisEntryValue::Bytes(b.clone()),
        },
        Value::SimpleString(s) => RedisEntryValue::Str(s.clone()),
        Value::Okay => RedisEntryValue::Str("OK".to_string()),
        other => RedisEntryValue::Str(format!("{other:?}")),
    }
}

fn summarize_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let after_userinfo = match rest.find('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    after_userinfo
        .split('?')
        .next()
        .unwrap_or(after_userinfo)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_plain_words() {
        let out = tokenize("HGETALL user:1").unwrap();
        assert_eq!(out, vec!["HGETALL", "user:1"]);
    }

    #[test]
    fn tokenize_double_quoted() {
        let out = tokenize(r#"HSET user:1 name "alice jones""#).unwrap();
        assert_eq!(out, vec!["HSET", "user:1", "name", "alice jones"]);
    }

    #[test]
    fn tokenize_single_quoted_preserves_double_inside() {
        let out = tokenize(r#"SET k 'a "b" c'"#).unwrap();
        assert_eq!(out, vec!["SET", "k", r#"a "b" c"#]);
    }

    #[test]
    fn tokenize_unterminated_quote_errors() {
        assert!(tokenize(r#"SET k "abc"#).is_err());
    }

    #[test]
    fn tokenize_backslash_escape_inside_quotes() {
        let out = tokenize(r#"SET k "line\nhere""#).unwrap();
        assert_eq!(out, vec!["SET", "k", "linenhere"]);
    }

    #[test]
    fn summarize_url_pulls_host() {
        assert_eq!(
            summarize_url("redis://:pw@r.example.com:6379/0"),
            "r.example.com:6379/0"
        );
        assert_eq!(summarize_url("redis://localhost:6379"), "localhost:6379");
    }

    #[test]
    fn interpret_hgetall_pairs() {
        let v = Value::Array(vec![
            Value::BulkString(b"name".to_vec()),
            Value::BulkString(b"alice".to_vec()),
            Value::BulkString(b"age".to_vec()),
            Value::BulkString(b"30".to_vec()),
        ]);
        let out = interpret("HGETALL", v, std::time::Duration::ZERO, 100).unwrap();
        match out {
            RedisReply::KeyValue { entries, .. } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].key, "name");
                assert_eq!(entries[0].value.as_display(), "alice");
            }
            _ => panic!("expected KeyValue"),
        }
    }

    #[test]
    fn interpret_ping_is_notice() {
        let v = Value::SimpleString("PONG".into());
        let out = interpret("PING", v, std::time::Duration::ZERO, 100).unwrap();
        assert!(matches!(out, RedisReply::Notice { .. }));
    }

    #[test]
    fn interpret_get_bulkstring_is_keyvalue() {
        let v = Value::BulkString(b"hello".to_vec());
        let out = interpret("GET", v, std::time::Duration::ZERO, 100).unwrap();
        match out {
            RedisReply::KeyValue { entries, .. } => {
                assert_eq!(entries[0].value.as_display(), "hello");
            }
            _ => panic!("expected KeyValue"),
        }
    }

    #[test]
    fn scan_extractor_reads_second_element() {
        let v = Value::Array(vec![
            Value::BulkString(b"0".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"user:1".to_vec()),
                Value::BulkString(b"user:2".to_vec()),
            ]),
        ]);
        let names = extract_scan_names(&v);
        assert_eq!(names, vec!["user:1", "user:2"]);
    }

    #[test]
    fn commands_include_common_verbs() {
        let cmds = redis_commands();
        for v in ["GET", "SET", "SCAN", "HGETALL"] {
            assert!(cmds.contains(&v), "missing {v}");
        }
    }
}
