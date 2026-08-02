//! Config for `mnml-aws-codebuild` — read-only CodeBuild project browser.
//!
//! 2026-07-21 rewrite. Previous version carried a rich `[[tabs]]`
//! schema (one tab per project or per log group) with a placeholder
//! `my-app` default that always errored with `ResourceNotFoundException`.
//! Now we auto-list projects and let the user optionally narrow to
//! a subset. Same shape as the sibling EventBridge Schedules
//! rewrite.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// AWS region to query. `None` → let the AWS CLI resolve from
    /// its own chain.
    #[serde(default)]
    pub region: Option<String>,
    /// When non-empty, only these projects appear (case-sensitive
    /// exact match against the project name from `list-projects`).
    /// Empty = show every project the account can see.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Number of recent builds to fetch per project. Default 5.
    #[serde(default = "default_recent")]
    pub recent_builds: usize,
}

fn default_recent() -> usize {
    5
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/mnml-aws-codebuild.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config {
            region: None,
            projects: Vec::new(),
            recent_builds: default_recent(),
        });
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg)
}
