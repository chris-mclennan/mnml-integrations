//! Config file at `~/.config/mnml-aws-sns.toml`. First run writes
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
    /// Tab kind: `topics` (every SNS topic in region) or `subscriptions`
    /// (subscriptions to a specific topic). Default = `topics`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Topic ARN — required when `kind = "subscriptions"`.
    #[serde(default)]
    pub topic_arn: Option<String>,
    /// Optional name prefix for `topics` tabs — filters to topics whose
    /// short name starts with this string. Useful for scoping in a
    /// shared account.
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

fn default_kind() -> String {
    "topics".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-sns config. Edit and re-run.
#
# Optional top-level region (defers to AWS CLI when unset):
# region = "us-east-1"

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "topics"        — every SNS topic in the region (default)
#   "subscriptions" — subscriptions to a specific topic (requires
#                     `topic_arn`)

[[tabs]]
name = "Topics"
kind = "topics"

# Example: scope by name prefix —
# [[tabs]]
# name = "billing topics"
# kind = "topics"
# prefix = "billing-"

# Example subscriptions tab — uncomment + set the topic ARN:
# [[tabs]]
# name = "orders subs"
# kind = "subscriptions"
# topic_arn = "arn:aws:sns:us-east-1:111111111111:orders-created"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "topics" => {}
                "subscriptions" => {
                    if t.topic_arn.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"subscriptions\" requires `topic_arn`",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"topics\" or \"subscriptions\")",
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
        .join("mnml-aws-sns.toml")
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
                topic_arn: None,
                prefix: None,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_subscriptions_without_topic_arn() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "subscriptions".into(),
                topic_arn: None,
                prefix: None,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
