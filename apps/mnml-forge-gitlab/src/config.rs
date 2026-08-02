//! Config file at `~/.config/mnml-forge-gitlab.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// API base URL — defaults to `https://gitlab.com/api/v4` for
    /// gitlab.com. Override for self-hosted (e.g.
    /// `https://gitlab.mycorp.com/api/v4`).
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_base_url() -> String {
    "https://gitlab.com/api/v4".to_string()
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// What kind of view this tab shows. `merge_requests` (default)
    /// or `pipelines`. MR-specific fields (`state`, `mode`) and
    /// pipeline-specific fields (`ref_name`) are ignored for the
    /// other kind.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Project — either `"group/path"` (URL form) or a numeric ID.
    /// Required for `pipelines` and for `merge_requests` without
    /// `mode`. Ignored for `merge_requests` with
    /// `mode = "mine"` / `"reviewing"` (those span all projects).
    #[serde(default)]
    pub project: Option<String>,
    /// MR state filter — `opened` (default), `closed`, `merged`,
    /// `all`. Ignored when `kind != "merge_requests"`.
    #[serde(default = "default_state")]
    pub state: String,
    /// Optional mode for cross-project MR tabs:
    ///   - omitted ⇒ literal per-project lookup (needs `project`)
    ///   - `mine` ⇒ MRs you authored across the instance
    ///   - `reviewing` ⇒ MRs where you are a reviewer
    ///
    /// Both auto-modes call `/user` at startup to resolve the
    /// current user's `id`. Ignored when `kind != "merge_requests"`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional branch filter for `pipelines` tabs. None ⇒ all
    /// branches (most-recently-updated first).
    #[serde(default)]
    pub ref_name: Option<String>,
}

fn default_state() -> String {
    "opened".to_string()
}

fn default_kind() -> String {
    "merge_requests".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-forge-gitlab config. Edit and re-run.
#
# Optional:
#   base_url  — defaults to gitlab.com. Override for self-hosted:
#               base_url = "https://gitlab.mycorp.com/api/v4"

# base_url = "https://gitlab.com/api/v4"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switched via 1-9 keys (or click)
# and rendered left→right.
#
# `kind` defaults to `merge_requests`. Supported kinds:
#   merge_requests — MR list with `state` + optional `mode`
#                    (`mine` / `reviewing` span all projects you
#                    have access to)
#   pipelines      — recent pipelines for a single `project`,
#                    optionally narrowed via `ref_name`

[[tabs]]
name = "Mine"
mode = "mine"

[[tabs]]
name = "Reviewing"
mode = "reviewing"

[[tabs]]
name = "your-project MRs"
project = "your-group/your-project"
state = "opened"

[[tabs]]
name = "your-project pipelines"
kind = "pipelines"
project = "your-group/your-project"

# Branch-filtered pipelines tab.
# [[tabs]]
# name = "main pipelines"
# kind     = "pipelines"
# project  = "your-group/your-project"
# ref_name = "main"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.base_url.trim().is_empty() {
            return Err(anyhow!("config: `base_url` cannot be empty"));
        }
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let valid_kind = matches!(t.kind.as_str(), "merge_requests" | "pipelines");
            if !valid_kind {
                return Err(anyhow!(
                    "tab #{i} ({}): kind must be `merge_requests` or `pipelines`, got `{}`",
                    t.name,
                    t.kind
                ));
            }
            match t.kind.as_str() {
                "merge_requests" => {
                    let valid_state =
                        matches!(t.state.as_str(), "opened" | "closed" | "merged" | "all");
                    if !valid_state {
                        return Err(anyhow!(
                            "tab #{i} ({}): state must be opened / closed / merged / all, got `{}`",
                            t.name,
                            t.state
                        ));
                    }
                    if let Some(mode) = &t.mode {
                        if mode != "mine" && mode != "reviewing" {
                            return Err(anyhow!(
                                "tab #{i} ({}): mode must be `mine` or `reviewing`, got `{mode}`",
                                t.name
                            ));
                        }
                    } else if t.project.is_none() {
                        return Err(anyhow!(
                            "tab #{i} ({}): one of `mode` or `project` is required for `merge_requests`",
                            t.name
                        ));
                    }
                }
                "pipelines" => {
                    if t.project.is_none() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `project` is required for kind `pipelines`",
                            t.name
                        ));
                    }
                }
                _ => unreachable!("kind validity already checked above"),
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-forge-gitlab.toml")
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
    fn validate_rejects_mr_tab_without_mode_or_project() {
        let raw = r##"
[[tabs]]
name = "Bad"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_pipelines_without_project() {
        let raw = r##"
[[tabs]]
name = "Bad"
kind = "pipelines"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("project"));
    }

    #[test]
    fn validate_rejects_bad_state() {
        let raw = r##"
[[tabs]]
name = "Bad"
mode  = "mine"
state = "PENDING"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_mode() {
        let raw = r##"
[[tabs]]
name = "Bad"
mode = "haha"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_pipelines_with_ref_name() {
        let raw = r##"
[[tabs]]
name     = "Main CI"
kind     = "pipelines"
project  = "group/project"
ref_name = "main"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.tabs[0].ref_name.as_deref(), Some("main"));
    }

    #[test]
    fn validate_rejects_no_tabs() {
        let raw = r##"
refresh_interval_secs = 30
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_base_url() {
        let raw = r##"
base_url = ""
[[tabs]]
name = "X"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn default_base_url_points_at_gitlab_com() {
        let raw = r##"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.base_url, "https://gitlab.com/api/v4");
    }
}
