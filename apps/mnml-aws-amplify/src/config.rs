//! Config file at `~/.config/mnml-aws-amplify.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Optional default region — overridden per-tab via `region`.
    #[serde(default)]
    pub region: Option<String>,
    /// Polling interval. `0` disables auto-refresh.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<Tab>,
    /// Amplify app ids to hide from the "All apps" tab. Useful for
    /// stale / decommissioned apps you don't want to scroll past.
    /// Get an id from `aws amplify list-apps` or the console URL.
    #[serde(default)]
    pub hidden_app_ids: Vec<String>,
    /// Amplify app ids in the user's preferred display order.
    /// Apps listed here appear first, in this exact order. Apps
    /// NOT listed appear after in whatever order AWS returned
    /// them. Written by Alt-↑ / Alt-↓ in-app; hand-edit is fine
    /// too.
    #[serde(default)]
    pub app_order: Vec<String>,
}

fn default_refresh() -> u64 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// `apps` (default) — list every Amplify app in the region.
    /// `app` — drill into one specific app's branches + deploy
    /// jobs. Requires `app_id`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Amplify app id (e.g. `d2abc123def456`). Required for `app`.
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

fn default_kind() -> String {
    "apps".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-amplify config. Edit and re-run.
#
# Optional top-level region (defers to AWS CLI when unset):
# region = "us-east-1"

refresh_interval_secs = 60

# Hide stale / decommissioned apps from the "All apps" list.
# Get an id from `aws amplify list-apps` or the Amplify console URL.
# hidden_app_ids = ["d1oldappxyz", "d1anotheroldapp"]

# ── Tabs ─────────────────────────────────────────────────────────
# Each [[tabs]] entry is one tab. Two kinds:
#   apps — list every Amplify app in the region (broad view)
#   app  — drill into one specific app's branches + deploy jobs
#
# Start with just the "All apps" tab. Add per-app tabs below once
# you know which apps you look at often — get the app_id from the
# Amplify console URL or `aws amplify list-apps`.

[[tabs]]
name = "All apps"
kind = "apps"

# [[tabs]]
# name = "Frontend"
# kind = "app"
# app_id = "d1lhq5v1rnado8"   # example: from the console URL
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "apps" => {}
                "app" => {
                    let id = t.app_id.as_deref().unwrap_or("").trim();
                    if id.is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `app_id` is required for kind `app`",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind `{other}` (use `apps` or `app`)",
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
        .join("mnml-aws-amplify.toml")
}

/// Write the current config back to `~/.config/mnml-aws-amplify.toml`.
/// Used by the runtime `x` / `H` toggles so hide-state persists
/// across restarts. Full rewrite (serde re-serializes the whole
/// document) — comments in the file are dropped, which is why the
/// initial template only carries them in the scaffold path.
pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg).map_err(|e| anyhow!("serialize config: {e}"))?;
    std::fs::write(&path, text)?;
    Ok(())
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
    fn example_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("parses");
        cfg.validate().expect("validates");
    }

    #[test]
    fn rejects_app_kind_without_app_id() {
        let raw = r##"
[[tabs]]
name = "bad"
kind = "app"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let raw = r##"
[[tabs]]
name = "bad"
kind = "garbage"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }
}
