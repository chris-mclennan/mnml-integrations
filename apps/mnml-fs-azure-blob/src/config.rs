//! Config file at `~/.config/mnml-fs-azure-blob.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Polling interval. `0` disables auto-refresh (the default for
    /// Azure Blob — listings don't change rapidly). User can still
    /// press `r` to refresh the active tab.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<TabConfig>,
}

fn default_refresh() -> u64 {
    0
}

/// What a tab opens on. Three flavours, mirroring the three
/// drill-down levels of Azure Blob Storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabKind {
    /// List storage accounts visible to the current `az` login.
    Accounts,
    /// List containers in a named storage account.
    Containers,
    /// List blobs in a container (optionally under a prefix).
    Blobs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabConfig {
    /// Human label shown in the tab strip.
    pub name: String,
    /// What this tab lists. See [`TabKind`].
    pub kind: TabKind,
    /// Required for `containers` / `blobs`. The storage account name
    /// (`mystorageacct`, not the full blob endpoint URL).
    #[serde(default)]
    pub account: Option<String>,
    /// Required for `blobs`. The container name (`logs`).
    #[serde(default)]
    pub container: Option<String>,
    /// Optional starting prefix for `blobs` tabs (`2026/` jumps into
    /// that subtree). Trailing slash matters.
    #[serde(default)]
    pub prefix: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-fs-azure-blob config. Edit and re-run.
#
# Optional global:
#   refresh_interval_secs  — default 0 (no auto-refresh). Blob
#                            listings don't churn, so the default is
#                            no-poll; press `r` in the TUI to refresh.

refresh_interval_secs = 0

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switch with 1-9 in the TUI.
#
# `kind` is one of:
#   - "accounts"   : list every storage account in your subscription
#   - "containers" : list containers in a named account
#   - "blobs"      : list blobs in a named container (optional prefix)
#
# Most setups start with one "accounts" tab and pin a couple of
# "blobs" tabs for the containers you actually browse daily.

[[tabs]]
name = "all accounts"
kind = "accounts"

[[tabs]]
name = "logs"
kind = "blobs"
account = "mystorageacct"
container = "logs"
# prefix = "2026/"

[[tabs]]
name = "exports"
kind = "containers"
account = "mystorageacct"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind {
                TabKind::Accounts => {
                    // Nothing else required.
                }
                TabKind::Containers => {
                    if t.account.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `account` is required for kind=\"containers\"",
                            t.name
                        ));
                    }
                }
                TabKind::Blobs => {
                    if t.account.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `account` is required for kind=\"blobs\"",
                            t.name
                        ));
                    }
                    if t.container.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `container` is required for kind=\"blobs\"",
                            t.name
                        ));
                    }
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
        .join("mnml-fs-azure-blob.toml")
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
        assert!(cfg.tabs.len() >= 3);
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            refresh_interval_secs: 0,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_blobs_without_container() {
        let raw = r##"
[[tabs]]
name = "bad"
kind = "blobs"
account = "mystorageacct"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("container"));
    }

    #[test]
    fn rejects_containers_without_account() {
        let raw = r##"
[[tabs]]
name = "bad"
kind = "containers"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("account"));
    }

    #[test]
    fn accounts_tab_needs_nothing_else() {
        let raw = r##"
[[tabs]]
name = "all"
kind = "accounts"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }
}
