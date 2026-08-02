//! Config file at `~/.config/mnml-forge-azdevops.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Default Azure DevOps organization (the `<org>` in
    /// `https://dev.azure.com/<org>/`). Tabs can override via
    /// per-row `org = "..."`.
    pub org: String,
    /// Default project. Tabs can override via per-row `project = "..."`.
    /// Optional at the top level — every tab must end up with one
    /// (either set on the row or inherited from here).
    #[serde(default)]
    pub project: Option<String>,
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
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
    /// What kind of view this tab shows. `pull_requests` (default)
    /// or `builds`. PR-specific fields (`state`, `mode`) and
    /// build-specific fields (`branch`, `definition`) are ignored
    /// for the other kind.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Override the default org for this tab.
    #[serde(default)]
    pub org: Option<String>,
    /// Override the default project for this tab.
    #[serde(default)]
    pub project: Option<String>,
    /// Repository name (the `<repo>` in
    /// `dev.azure.com/<org>/<project>/_git/<repo>`). Required for
    /// PR tabs unless `mode = "mine"` / `"reviewing"`. Also used
    /// to filter `builds` tabs to a single repo (optional there).
    #[serde(default)]
    pub repo: Option<String>,
    /// PR status filter — `active` (default), `completed`,
    /// `abandoned`, `all`. Ignored when `kind != "pull_requests"`.
    #[serde(default = "default_state")]
    pub state: String,
    /// Optional mode for cross-repo PR tabs:
    ///   - omitted ⇒ literal per-repo lookup (needs `repo`)
    ///   - `mine` ⇒ PRs you created in the project
    ///   - `reviewing` ⇒ PRs where you are a reviewer
    ///
    /// Both auto-modes call `/_apis/connectionData` at startup to
    /// resolve the current user's `id`. Ignored when
    /// `kind != "pull_requests"`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional pipeline-definition ID filter for `builds` tabs.
    /// None ⇒ all definitions in the project.
    #[serde(default)]
    pub definition: Option<i64>,
    /// Optional branch filter (full ref or short name — the worker
    /// accepts both). None ⇒ all branches.
    #[serde(default)]
    pub branch: Option<String>,
}

fn default_state() -> String {
    "active".to_string()
}

fn default_kind() -> String {
    "pull_requests".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-forge-azdevops config. Edit and re-run.
#
# Required:
#   org      — default Azure DevOps organization slug
#              (the <org> in https://dev.azure.com/<org>/)
# Optional:
#   project  — default project slug; tabs can override per-row

org     = "your-org"
project = "your-project"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switched via 1-9 keys (or click)
# and rendered left→right.
#
# `kind` defaults to `pull_requests`. Supported kinds:
#   pull_requests — PR list with `state`, `mode = mine|reviewing`
#   builds        — recent builds for a project (optional `repo`,
#                   `definition`, `branch` filters)

[[tabs]]
name = "Mine"
mode = "mine"

[[tabs]]
name = "Reviewing"
mode = "reviewing"

[[tabs]]
name = "your-repo PRs"
repo  = "your-repo"
state = "active"

[[tabs]]
name = "Builds"
kind = "builds"

# Per-repo builds tab, narrowed to main.
# [[tabs]]
# name = "your-repo main"
# kind   = "builds"
# repo   = "your-repo"
# branch = "main"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.org.trim().is_empty() {
            return Err(anyhow!("config: `org` is required"));
        }
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let valid_kind = matches!(t.kind.as_str(), "pull_requests" | "builds");
            if !valid_kind {
                return Err(anyhow!(
                    "tab #{i} ({}): kind must be `pull_requests` or `builds`, got `{}`",
                    t.name,
                    t.kind
                ));
            }
            // Project resolution: every tab must end up with one.
            let project = t.project.as_deref().or(self.project.as_deref());
            if project.is_none() || project.unwrap().trim().is_empty() {
                return Err(anyhow!(
                    "tab #{i} ({}): no `project` — set one at the row or in [config]",
                    t.name
                ));
            }
            match t.kind.as_str() {
                "pull_requests" => {
                    let valid_state =
                        matches!(t.state.as_str(), "active" | "completed" | "abandoned" | "all");
                    if !valid_state {
                        return Err(anyhow!(
                            "tab #{i} ({}): state must be active / completed / abandoned / all, got `{}`",
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
                    } else if t.repo.is_none() {
                        return Err(anyhow!(
                            "tab #{i} ({}): one of `mode` or `repo` is required for `pull_requests`",
                            t.name
                        ));
                    }
                }
                "builds" => {
                    // `builds` is project-scoped by default; repo/definition/
                    // branch are all optional narrowers. Nothing further to
                    // validate.
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
        .join("mnml-forge-azdevops.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        return Err(anyhow!(
            "wrote config template to {} — edit it (set org + project), then re-run",
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
        let mut cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.org = "acme".into();
        cfg.project = Some("Example".into());
        cfg.validate().expect("example validates after fill-in");
        assert!(cfg.tabs.len() >= 3);
    }

    #[test]
    fn validate_rejects_missing_org() {
        let raw = r##"
org = ""
project = "p"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_pr_without_repo_or_mode() {
        let raw = r##"
org = "o"
project = "p"
[[tabs]]
name = "Bad"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_state() {
        let raw = r##"
org = "o"
project = "p"
[[tabs]]
name = "Bad"
mode = "mine"
state = "PENDING"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_builds_kind() {
        let raw = r##"
org = "o"
project = "p"
[[tabs]]
name = "Builds"
kind = "builds"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_tab_without_project() {
        let raw = r##"
org = "o"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("project"));
    }

    #[test]
    fn tab_can_override_project_inline() {
        let raw = r##"
org = "o"
[[tabs]]
name = "Mine"
project = "p2"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_bad_mode() {
        let raw = r##"
org = "o"
project = "p"
[[tabs]]
name = "Bad"
mode = "haha"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_unknown_kind() {
        let raw = r##"
org = "o"
project = "p"
[[tabs]]
name = "Bad"
kind = "garbage"
repo = "r"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("pull_requests"));
    }
}
