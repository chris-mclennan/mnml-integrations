//! Config file at `~/.config/mnml-aws-sqs.toml`. First run writes
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
    /// Tab kind: `all` (every queue in region) or `prefix` (queues
    /// whose name starts with `prefix`). Default = `all`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Name prefix — consulted when `kind = "prefix"`. Useful for
    /// scoping to one app's queues in a shared account.
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

fn default_kind() -> String {
    "all".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-sqs config. Edit and re-run.
#
# Optional top-level region (defers to AWS CLI when unset):
# region = "us-east-1"

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "all"    — every queue in the region (default)
#   "prefix" — queues whose name starts with `prefix`

[[tabs]]
name = "All queues"
kind = "all"

# Example prefix tab — uncomment + set the prefix:
# [[tabs]]
# name = "ingest queues"
# kind = "prefix"
# prefix = "ingest-"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "all" => {}
                "prefix" => {
                    if t.prefix.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"prefix\" requires `prefix`",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"all\" or \"prefix\")",
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
        .join("mnml-aws-sqs.toml")
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
                prefix: None,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_prefix_without_value() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "prefix".into(),
                prefix: None,
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
