//! Config file at `~/.config/mnml-fs-s3.toml`. First run writes
//! the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Polling interval. `0` disables auto-refresh (the default for
    /// S3 — listings don't change rapidly). User can still press
    /// `r` to refresh the active tab.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Bucket list — at least one required.
    #[serde(default)]
    pub buckets: Vec<Bucket>,
}

fn default_refresh() -> u64 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    /// Human label shown in the tab strip.
    pub name: String,
    /// S3 bucket name (`my-app-logs`, not `s3://my-app-logs/`).
    pub bucket: String,
    /// Optional starting prefix (`2026/` shows only that subtree).
    /// Trailing slash matters — `2026` would match `2026.zip` too.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Optional region override — defaults to the AWS CLI's
    /// resolved region (env var, profile, etc.).
    #[serde(default)]
    pub region: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-fs-s3 config. Edit and re-run.
#
# Optional global:
#   refresh_interval_secs  — default 0 (no auto-refresh). S3 listings
#                            don't churn, so the default is no-poll;
#                            press `r` in the TUI to refresh.

refresh_interval_secs = 0

# ── Buckets ──────────────────────────────────────────────────────
# Each `[[buckets]]` entry is one tab. Switch with 1-9 in the TUI.
# Region is optional — defaults to the `aws` CLI's resolved region.
# Prefix is optional — `2026/06/` jumps straight into that subtree.

[[buckets]]
name = "logs"
bucket = "my-app-logs"
# prefix = "2026/"
# region = "us-east-1"

[[buckets]]
name = "exports"
bucket = "my-data-exports"

[[buckets]]
name = "configs"
bucket = "my-app-configs"
prefix = "prod/"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.buckets.is_empty() {
            return Err(anyhow!("config: at least one [[buckets]] entry required"));
        }
        for (i, b) in self.buckets.iter().enumerate() {
            if b.bucket.trim().is_empty() {
                return Err(anyhow!("bucket #{i} ({}): `bucket` is required", b.name));
            }
            if b.bucket.contains('/') || b.bucket.starts_with("s3:") {
                return Err(anyhow!(
                    "bucket #{i} ({}): use a bare bucket name, not `s3://…` or `bucket/prefix`",
                    b.name
                ));
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-fs-s3.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        return Err(anyhow!(
            "wrote config template to {} — edit it then re-run",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert!(cfg.buckets.len() >= 3);
    }

    #[test]
    fn rejects_no_buckets() {
        let cfg = Config {
            refresh_interval_secs: 0,
            buckets: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_s3_uri_in_bucket_field() {
        let raw = r##"
[[buckets]]
name = "bad"
bucket = "s3://my-bucket/"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("bare bucket name"));
    }

    #[test]
    fn rejects_bucket_with_slash() {
        let raw = r##"
[[buckets]]
name = "bad"
bucket = "my-bucket/sub-prefix"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("bare bucket name"));
    }
}
