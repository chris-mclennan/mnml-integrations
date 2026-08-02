//! Config for `mnml-aws-eventbridge` — EventBridge Scheduler MVP.
//!
//! Prior versions carried a rich `[[tabs]]` schema for the
//! Rules-per-bus view. The 2026-07-21 rewrite drops that; the MVP
//! is a single flat schedules view. Config now only holds the
//! optional region override.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// AWS region to query. `None` → let the AWS CLI resolve from
    /// its own chain (env, profile, `~/.aws/config`).
    #[serde(default)]
    pub region: Option<String>,
    /// 2026-08-01 — dot-notation JSON path into the schedule's
    /// target Input for the ENV column. Empty / unset = column
    /// shows a dash. Example: input JSON
    ///   { "job": { "env": "prod" } }
    /// → env_path = "job.env" → column shows "prod". Deliberately
    /// generic — no product-specific defaults. Configure per your
    /// target shape.
    #[serde(default)]
    pub env_path: String,
    /// 2026-08-01 — same as env_path but for the BRANCH column.
    #[serde(default)]
    pub branch_path: String,
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/mnml-aws-eventbridge.toml")
}

/// 2026-08-01 — scaffold config written on first run when the
/// user has no config yet. Contains commented-out `env_path`
/// and `branch_path` examples so the ENV / BRANCH columns are
/// discoverable. Region stays optional (aws CLI resolution).
pub const SCAFFOLD: &str = r##"# mnml-aws-eventbridge config.
#
# AWS region to query. Leave unset to let the aws CLI resolve
# (env vars, profile, ~/.aws/config).
# region = "us-east-1"

# ── ENV / BRANCH columns ──────────────────────────────────────
# Two extra columns in the schedules table extract values from
# each schedule's target Input JSON. Configure the dot-path
# to the field in your target-input shape. Unset (or empty) =
# the column shows a dash for every schedule.
#
# Example: if your target input JSON is
#   { "job": { "env": "prod", "branch": "main" } }
# then set:
#   env_path    = "job.env"
#   branch_path = "job.branch"
#
# Deliberately generic — no product-specific defaults. Different
# schedules with different target shapes just show a dash where
# the path misses.
#
# env_path    = ""
# branch_path = ""
"##;

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        // Best-effort scaffold write on first run — same pattern
        // as mnml-msg-slack. Failures fall through to a default
        // config so we never block startup on a bad ~/.config
        // permissions setup.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, SCAFFOLD);
        return Ok(Config::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg)
}

/// 2026-08-01 — walk a dot-notation JSON path into a target-input
/// JSON string. Returns None when: path is empty, JSON parse
/// fails, any intermediate key is missing, or the leaf value
/// isn't a scalar (string/number/bool). Called by the ENV /
/// BRANCH column render — a None → the column shows a dash.
pub fn extract_json_path(input: &str, dot_path: &str) -> Option<String> {
    if dot_path.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let mut cur = &v;
    for part in dot_path.split('.') {
        cur = cur.get(part)?;
    }
    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_path_walks_nested() {
        let j = r#"{"job":{"env":"prod","branch":"main"}}"#;
        assert_eq!(extract_json_path(j, "job.env"), Some("prod".into()));
        assert_eq!(extract_json_path(j, "job.branch"), Some("main".into()));
    }

    #[test]
    fn extract_json_path_top_level() {
        let j = r#"{"env":"staging"}"#;
        assert_eq!(extract_json_path(j, "env"), Some("staging".into()));
    }

    #[test]
    fn extract_json_path_missing_returns_none() {
        let j = r#"{"job":{"env":"prod"}}"#;
        assert_eq!(extract_json_path(j, "job.missing"), None);
        assert_eq!(extract_json_path(j, "missing.env"), None);
    }

    #[test]
    fn extract_json_path_non_scalar_returns_none() {
        // Nested-object leaves aren't renderable as one column
        // cell — return None so the caller shows a dash.
        let j = r#"{"job":{"env":{"nested":"x"}}}"#;
        assert_eq!(extract_json_path(j, "job.env"), None);
    }

    #[test]
    fn extract_json_path_scalar_types() {
        let j = r#"{"count":42,"active":true,"name":"x"}"#;
        assert_eq!(extract_json_path(j, "count"), Some("42".into()));
        assert_eq!(extract_json_path(j, "active"), Some("true".into()));
        assert_eq!(extract_json_path(j, "name"), Some("x".into()));
    }

    #[test]
    fn extract_json_path_empty_path_returns_none() {
        assert_eq!(extract_json_path(r#"{"env":"x"}"#, ""), None);
    }

    #[test]
    fn extract_json_path_bad_json_returns_none() {
        assert_eq!(extract_json_path("not json", "env"), None);
    }

    #[test]
    fn scaffold_parses_as_config() {
        let cfg: Config = toml::from_str(SCAFFOLD).expect("scaffold TOML valid");
        // All keys commented out in the scaffold — defaults expected.
        assert!(cfg.env_path.is_empty());
        assert!(cfg.branch_path.is_empty());
        assert!(cfg.region.is_none());
    }
}
