//! Config file at `~/.config/mnml-cdn-cloudflare/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`CLOUDFLARE_API_TOKEN`,
//! `CLOUDFLARE_ACCOUNT_ID`) — never in the TOML.

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
    ///   - `zones` — every zone in the account
    ///   - `dns` — DNS records for a specific zone (requires `zone_id`)
    ///   - `workers` — Worker scripts (requires `CLOUDFLARE_ACCOUNT_ID`)
    ///   - `pages` — Pages projects (requires `CLOUDFLARE_ACCOUNT_ID`)
    ///   - `security_events` — recent firewall events for one zone
    ///     (requires `zone_id`)
    pub kind: String,
    /// Required for `dns` and `security_events`.
    #[serde(default)]
    pub zone_id: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-cdn-cloudflare config. Edit and re-run.
#
# Auth lives in env vars (NOT here):
#   export CLOUDFLARE_API_TOKEN=...      (required)
#   export CLOUDFLARE_ACCOUNT_ID=...     (required for workers / pages tabs)
#
# Token: create at dash.cloudflare.com → My Profile → API Tokens.
# Account ID: dash.cloudflare.com → sidebar → Account ID.

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "zones"            — every zone (status / plan / paused)
#   "dns"              — DNS records for one zone (requires `zone_id`)
#   "workers"          — Worker scripts (account-scoped)
#   "pages"            — Pages projects (account-scoped)
#   "security_events"  — firewall events for one zone (requires `zone_id`)

[[tabs]]
name = "Zones"
kind = "zones"

# Per-zone DNS — set zone_id to enable:
# [[tabs]]
# name = "example.com DNS"
# kind = "dns"
# zone_id = "abc123..."

[[tabs]]
name = "Workers"
kind = "workers"

[[tabs]]
name = "Pages"
kind = "pages"

# Per-zone security events — set zone_id to enable:
# [[tabs]]
# name = "example.com WAF"
# kind = "security_events"
# zone_id = "abc123..."
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "zones" | "workers" | "pages" => {}
                "dns" | "security_events" => {
                    if t.zone_id.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"{}\" requires `zone_id`",
                            t.name,
                            t.kind
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"zones\", \"dns\", \"workers\", \"pages\", or \"security_events\")",
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
        .join("mnml-cdn-cloudflare")
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
                zone_id: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_dns_without_zone_id() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "dns".into(),
                zone_id: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_security_events_without_zone_id() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "security_events".into(),
                zone_id: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
