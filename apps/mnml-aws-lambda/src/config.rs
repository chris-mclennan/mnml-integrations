//! Config file at `~/.config/mnml-aws-lambda.toml`. First
//! run writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Optional default region — overridden per-tab via `region`.
    /// Defers to the AWS CLI's resolution chain when unset.
    #[serde(default)]
    pub region: Option<String>,
    /// Background refresh cadence in seconds. 0 disables auto-refresh
    /// (manual `r` only).
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// Tab kind: `all` (every function in region) or `watched`
    /// (explicit list of function names). Default = `all`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Function name list — only consulted when `kind = "watched"`.
    #[serde(default)]
    pub watched: Vec<String>,
    /// Optional region override for this tab.
    #[serde(default)]
    pub region: Option<String>,
}

fn default_kind() -> String {
    "all".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-lambda config. Edit and re-run.
#
# Optional top-level region (defers to AWS CLI when unset):
# region = "us-east-1"

# Auto-refresh cadence in seconds (0 disables).
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Each [[tabs]] entry is one list view. Switch with 1-9 in the TUI.
#
# Kinds:
#   "all"     — every function in the region (default)
#   "watched" — explicit list of function names (the `watched` array)

[[tabs]]
name = "All"
kind = "all"

[[tabs]]
name = "Watched"
kind = "watched"
watched = [
  "api-handler",
  "ingest-worker",
]
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "all" => {}
                "watched" => {
                    if t.watched.is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"watched\" requires a non-empty `watched` array",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"all\" or \"watched\")",
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
        .join("mnml-aws-lambda.toml")
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
        assert!(cfg.tabs.len() >= 2);
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
                watched: vec![],
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_watched_without_entries() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "watched".into(),
                kind: "watched".into(),
                watched: vec![],
                region: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn default_kind_is_all() {
        let cfg: Config = toml::from_str(
            r#"
            [[tabs]]
            name = "X"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.tabs[0].kind, "all");
    }
}
