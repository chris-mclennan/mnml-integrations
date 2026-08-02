//! Config file at `~/.config/mnml-obs-datadog/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`DD_API_KEY`, `DD_APP_KEY`, `DD_SITE`)
//! — never in the TOML.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
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
    /// Tab kind:
    ///   - `monitors` — every monitor (optional `tag` filter)
    ///   - `dashboards` — every dashboard (optional `query` filter)
    ///   - `logs` — recent logs matching `query` (live-tails when focused)
    ///   - `incidents` — open (active) incidents
    pub kind: String,
    /// `monitors`: tag scope (e.g. `service:api`). Optional.
    /// `dashboards`: title prefix filter. Optional.
    /// `logs`: REQUIRED — the Datadog logs query.
    /// `incidents`: ignored.
    #[serde(default)]
    pub query: Option<String>,
    /// `logs`-only: time window (e.g. `"now-15m"`). Defaults to
    /// `now-15m` when unset.
    #[serde(default)]
    pub from: Option<String>,
    /// `logs`-only: poll interval override. Defaults to 5s when unset.
    #[serde(default)]
    pub tail_interval_secs: Option<u64>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-obs-datadog config. Edit and re-run.
#
# Auth lives in env vars (NOT here):
#   export DD_API_KEY=...      (required)
#   export DD_APP_KEY=...      (required)
#   export DD_SITE=datadoghq.com   (defaults to the US1 site)
#
# Other DD_SITE values: datadoghq.eu, us3.datadoghq.com,
# us5.datadoghq.com, ap1.datadoghq.com, ddog-gov.com.

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "monitors"    — every monitor (color-coded by alert state)
#   "dashboards"  — every dashboard (id, title, author)
#   "logs"        — live-tail logs matching `query`
#   "incidents"   — open (active) incidents

[[tabs]]
name = "Monitors"
kind = "monitors"

# Scope monitors by tag —
# [[tabs]]
# name = "api alerts"
# kind = "monitors"
# query = "tag:service:api"

[[tabs]]
name = "Dashboards"
kind = "dashboards"

# A logs live-tail tab — `query` uses Datadog log search syntax:
[[tabs]]
name = "API errors"
kind = "logs"
query = "service:api status:error"
from = "now-15m"
tail_interval_secs = 5

[[tabs]]
name = "Incidents"
kind = "incidents"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "monitors" | "dashboards" | "incidents" => {}
                "logs" => {
                    if t.query.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"logs\" requires `query`",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"monitors\", \"dashboards\", \"logs\", or \"incidents\")",
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
        .join("mnml-obs-datadog")
        .join("config.toml")
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
            refresh_interval_secs: 60,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
                query: None,
                from: None,
                tail_interval_secs: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_logs_without_query() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "logs".into(),
                query: None,
                from: None,
                tail_interval_secs: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
