//! Config load / save. First run writes a scaffold and exits with
//! setup instructions.
//!
//! Layout:
//!   `~/.config/mnml-db/connections.toml` — a list of ConnectionSpecs.
//!   `~/.config/mnml-db/secrets.toml`     — reserved for phase 2+
//!                                          (keychain fallback for
//!                                          hosts without env vars).
//!
//! No passwords are stored in-file — v0.1 rejects any spec whose
//! creds section is a `plaintext` variant.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::connection::ConnectionSpec;
use crate::drivers::supported_engines;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Row cap per query. Doubled at runtime by pressing `R`.
    #[serde(default = "default_row_limit")]
    pub row_limit: u32,
    #[serde(default, rename = "connection")]
    pub connections: Vec<ConnectionSpec>,
}

fn default_row_limit() -> u32 {
    500
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-db config. Edit and re-run.
#
# Connection specs live here. Passwords never do — v0.1 rejects
# any `password = ...` field. Reference a password by env-var name
# or by macOS keychain entry via `[connection.creds]`.

row_limit = 500

# --- Postgres example -------------------------------------------
[[connection]]
id = "local-pg"
label = "Local Postgres"
engine = "postgres"
host = "localhost"
port = 5432
user = "postgres"
database = "postgres"
# [connection.creds]
# type = "env"
# password = "PGPASSWORD"

# --- Redis example ----------------------------------------------
[[connection]]
id = "local-redis"
label = "Local Redis"
engine = "redis"
host = "localhost"
port = 6379
database = "0"
# [connection.creds]
# type = "env"
# password = "REDIS_PASSWORD"

# --- MariaDB / MySQL example ------------------------------------
# The `mariadb` driver crate speaks the MySQL wire protocol, so
# `engine = "mysql"` is accepted as an alias for `engine = "mariadb"`.
# [[connection]]
# id = "local-mariadb"
# label = "Local MariaDB"
# engine = "mariadb"
# host = "localhost"
# port = 3306
# user = "root"
# database = "app"
# [connection.creds]
# type = "env"
# password = "MARIADB_PASSWORD"

# --- ClickHouse example -----------------------------------------
# Talks to the ClickHouse HTTP endpoint. Set `[connection.params]
# scheme = "https"` for TLS clusters (default port switches to 8443).
# [[connection]]
# id = "local-clickhouse"
# label = "Local ClickHouse"
# engine = "clickhouse"
# host = "localhost"
# port = 8123
# user = "default"
# database = "default"
# [connection.creds]
# type = "env"
# password = "CLICKHOUSE_PASSWORD"
# [connection.params]
# scheme = "http"

# --- Redshift example -------------------------------------------
# Redshift speaks the Postgres wire protocol; catalog queries use
# the Redshift-specific `svv_all_*` views.
# [[connection]]
# id = "warehouse"
# label = "AWS Redshift"
# engine = "redshift"
# host = "dw.abc.us-east-1.redshift.amazonaws.com"
# port = 5439
# user = "awsuser"
# database = "warehouse"
# [connection.creds]
# type = "env"
# password = "REDSHIFT_PASSWORD"

# --- DocumentDB / MongoDB example -------------------------------
# The URI carries the full auth + TLS story; keep it in an env var
# so the config file stays password-free. `params.uri` inline is
# fine only when the URI has no password (a keyfile / SSO cluster).
# [[connection]]
# id = "docdb-prod"
# label = "DocDB Prod"
# engine = "docdb"
# [connection.creds]
# type = "env"
# password = "DOCDB_URI"
# # [connection.params]
# # uri = "mongodb://cluster.example.com:27017/app?tls=true&replicaSet=rs0&retryWrites=false"

# --- DynamoDB example -------------------------------------------
# Credentials come from the standard AWS CLI chain (profile /
# ~/.aws/credentials / IMDS / SSO). `AWS_ACCESS_KEY_ID` +
# `AWS_SECRET_ACCESS_KEY` env vars take precedence per aws CLI
# convention. This driver never touches those directly.
# [[connection]]
# id = "dynamo-dev"
# label = "DynamoDB Dev"
# engine = "dynamodb"
# [connection.params]
# profile = "dev"
# region = "us-east-1"

# --- Keychain example (macOS) -----------------------------------
# [[connection]]
# id = "staging-pg"
# label = "Staging Postgres"
# engine = "postgres"
# host = "db.staging.example.com"
# port = 5432
# user = "api_readonly"
# database = "api"
# [connection.creds]
# type = "keychain"
# service = "mnml-db-staging-pg"
# account = "api_readonly"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.row_limit == 0 {
            return Err(anyhow!("config: row_limit must be > 0"));
        }
        if self.connections.is_empty() {
            return Err(anyhow!(
                "config: at least one [[connection]] entry required"
            ));
        }
        let supported = supported_engines();
        let mut seen = std::collections::HashSet::new();
        for c in &self.connections {
            if c.id.trim().is_empty() {
                return Err(anyhow!("connection: `id` is required"));
            }
            if !seen.insert(c.id.clone()) {
                return Err(anyhow!("connection `{}`: duplicate id", c.id));
            }
            if !supported.contains(&c.engine.as_str()) {
                return Err(anyhow!(
                    "connection `{}`: engine `{}` isn't compiled in (supported: {})",
                    c.id,
                    c.engine,
                    supported.join(", ")
                ));
            }
        }
        Ok(())
    }
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-db")
}

pub fn config_path() -> PathBuf {
    config_dir().join("connections.toml")
}

/// Load + validate the config. On first run writes a scaffold and
/// returns an instructional error so the caller can print + exit.
pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).context("creating config dir")?;
        std::fs::write(&path, Config::EXAMPLE).context("writing scaffold config")?;
        // tester 2026-07-31 SEV-3 — was writing 0644 (world-readable)
        // despite the scaffold telling the user to chmod 600. Set the
        // secure perms up front so the tool practices what it preaches.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        return Err(anyhow!(
            "wrote config scaffold to {} (chmod 600) — edit it then re-run",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path).context("reading config")?;
    parse(&text)
}

/// Parse config text (also used by tests). Validates + rejects
/// plaintext creds.
pub fn parse(text: &str) -> Result<Config> {
    // Re-parse as a raw toml::Value so the plaintext-password
    // validator can inspect the original table.
    let raw: toml::Value = toml::from_str(text).context("parsing config as TOML")?;
    // tester 2026-07-31 SEV-3 — plaintext-password check MUST run
    // before the strict `deny_unknown_fields` deserialize. Otherwise
    // a top-level `password = "…"` field trips serde's "unknown
    // field" error and users see a confusing "unknown field" toast
    // instead of the intended "plaintext refused" message.
    if let Some(list) = raw.get("connection").and_then(|v| v.as_array()) {
        for raw_c in list.iter() {
            ConnectionSpec::validate_no_plaintext_password_raw(raw_c)?;
        }
    }
    let cfg: Config = toml::from_str(text).context("interpreting config")?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        // The example config is annotated as documentation — only
        // enable the ones for engines the current build supports.
        let cfg = parse(Config::EXAMPLE).unwrap();
        assert!(!cfg.connections.is_empty());
        assert_eq!(cfg.row_limit, 500);
    }

    #[test]
    fn empty_connections_rejected() {
        let raw = "row_limit = 100";
        let err = parse(raw).unwrap_err().to_string();
        assert!(err.contains("at least one"), "err: {err}");
    }

    #[test]
    fn zero_row_limit_rejected() {
        let raw = r##"
row_limit = 0
[[connection]]
id = "x"
engine = "postgres"
"##;
        assert!(parse(raw).is_err());
    }

    #[test]
    fn duplicate_ids_rejected() {
        let raw = r##"
[[connection]]
id = "same"
engine = "postgres"
[[connection]]
id = "same"
engine = "redis"
"##;
        let err = parse(raw).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "err: {err}");
    }

    #[test]
    fn plaintext_password_field_rejected() {
        let raw = r##"
[[connection]]
id = "x"
engine = "postgres"
password = "hunter2"
"##;
        let err = parse(raw).unwrap_err().to_string();
        assert!(err.contains("plaintext"), "err: {err}");
    }

    #[test]
    fn unknown_engine_rejected() {
        let raw = r##"
[[connection]]
id = "x"
engine = "duckdb"
"##;
        let err = parse(raw).unwrap_err().to_string();
        assert!(err.contains("compiled in"), "err: {err}");
    }
}
