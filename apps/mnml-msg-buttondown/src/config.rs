//! Config file at `~/.config/mnml-msg-buttondown/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`BUTTONDOWN_API_KEY`) — never in the
//! TOML.

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
    ///   - `drafts`      — unsent drafts (status=draft)
    ///   - `sent`        — already-sent emails (status=sent)
    ///   - `scheduled`   — emails scheduled for future send (status=scheduled)
    ///   - `subscribers` — every subscriber
    pub kind: String,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-msg-buttondown config. Edit and re-run.
#
# Auth lives in an env var (NOT here):
#   export BUTTONDOWN_API_KEY=...   (Settings → Programming in Buttondown)

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "drafts"      — unsent drafts (publish from the TUI with `p`)
#   "sent"        — already-shipped emails with open/click stats
#   "scheduled"   — queued for a future send
#   "subscribers" — every subscriber (unsubscribe with `X`)

[[tabs]]
name = "drafts"
kind = "drafts"

[[tabs]]
name = "sent"
kind = "sent"

[[tabs]]
name = "scheduled"
kind = "scheduled"

[[tabs]]
name = "subscribers"
kind = "subscribers"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "drafts" | "sent" | "scheduled" | "subscribers" => {}
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"drafts\", \"sent\", \"scheduled\", or \"subscribers\")",
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
        .join("mnml-msg-buttondown")
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
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_all_known_kinds() {
        for kind in ["drafts", "sent", "scheduled", "subscribers"] {
            let cfg = Config {
                refresh_interval_secs: 60,
                tabs: vec![Tab {
                    name: kind.into(),
                    kind: kind.into(),
                }],
            };
            cfg.validate().expect("kind accepted");
        }
    }
}
