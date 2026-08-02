//! Config file at `~/.config/mnml-aws-cognito.toml`. First run writes
//! the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// `pools` (all user pools) or `users` (recent users in a pool —
    /// requires `user_pool_id`).
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Cognito User Pool ID — required for `kind = "users"`.
    #[serde(default)]
    pub user_pool_id: Option<String>,
    /// Cap on how many users to list. Default 60. Cognito's
    /// list-users API caps at 60 per page; this knob controls how
    /// many pages we walk before stopping.
    #[serde(default = "default_user_limit")]
    pub user_limit: u32,
    #[serde(default)]
    pub region: Option<String>,
}

fn default_kind() -> String {
    "pools".to_string()
}

fn default_user_limit() -> u32 {
    60
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-cognito config. Edit and re-run.
#
# Optional top-level region (defers to AWS CLI when unset):
# region = "us-east-1"

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "pools" — every Cognito User Pool in the region (default)
#   "users" — recent users in a specific pool (requires `user_pool_id`)

[[tabs]]
name = "Pools"
kind = "pools"

# Example users tab — uncomment + set the pool ID:
# [[tabs]]
# name = "Recent users"
# kind = "users"
# user_pool_id = "us-east-1_abc123"
# user_limit = 60
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "pools" => {}
                "users" => {
                    if t.user_pool_id.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"users\" requires `user_pool_id`",
                            t.name
                        ));
                    }
                    if t.user_limit == 0 || t.user_limit > 600 {
                        return Err(anyhow!(
                            "tab #{i} ({}): user_limit must be in 1..=600 (Cognito caps a page at 60)",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"pools\" or \"users\")",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-aws-cognito.toml")
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
        assert!(!cfg.tabs.is_empty());
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
                user_pool_id: None,
                user_limit: 60,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_users_without_pool_id() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "users".into(),
                user_pool_id: None,
                user_limit: 60,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_user_limit_zero_or_over_max() {
        let mut cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "users".into(),
                user_pool_id: Some("us-east-1_abc".into()),
                user_limit: 0,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
        cfg.tabs[0].user_limit = 1000;
        assert!(cfg.validate().is_err());
    }
}
