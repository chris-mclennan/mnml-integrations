//! Config file at `~/.config/mnml-msg-mandrill/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`MANDRILL_API_KEY`) — never in TOML.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_lookback_days")]
    pub messages_lookback_days: u64,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

fn default_lookback_days() -> u64 {
    14
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// Tab kind:
    ///   - `messages`  — recent transactional sends
    ///   - `templates` — every template
    ///   - `tags`      — tag stats
    ///   - `webhooks`  — every webhook
    pub kind: String,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-msg-mandrill config. Edit and re-run.
#
# Auth lives in env (NOT here):
#   export MANDRILL_API_KEY=...   (required; Mandrill → Settings → API Keys)

refresh_interval_secs = 60
messages_lookback_days = 14

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "messages"  — recent transactional sends
#   "templates" — every template + publish state
#   "tags"      — every tag with cumulative stats + bounce rate
#   "webhooks"  — every webhook + events + auth-key tail

[[tabs]]
name = "messages"
kind = "messages"

[[tabs]]
name = "templates"
kind = "templates"

[[tabs]]
name = "tags"
kind = "tags"

[[tabs]]
name = "webhooks"
kind = "webhooks"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        if self.messages_lookback_days == 0 {
            return Err(anyhow!("config: messages_lookback_days must be at least 1"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "messages" | "templates" | "tags" | "webhooks" => {}
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"messages\", \"templates\", \"tags\", or \"webhooks\")",
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
        .join("mnml-msg-mandrill")
        .join("config.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    let first_run = !path.exists();
    if first_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        println!(
            "first run: wrote config template to {} — edit it to customize",
            path.display(),
        );
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
        assert_eq!(cfg.tabs.len(), 4);
        assert_eq!(cfg.messages_lookback_days, 14);
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            refresh_interval_secs: 60,
            messages_lookback_days: 14,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 60,
            messages_lookback_days: 14,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_zero_lookback() {
        let cfg = Config {
            refresh_interval_secs: 60,
            messages_lookback_days: 0,
            tabs: vec![Tab {
                name: "x".into(),
                kind: "messages".into(),
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
