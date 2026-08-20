//! Config file at `~/.config/mnml-forge-github.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required. `#[serde(default)]` lets a
    /// config-with-no-tabs parse so `validate()` can produce a
    /// human-readable error rather than the cryptic
    /// `missing field "tabs"` from serde.
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
    /// What kind of view this tab shows. `issues` (default — search
    /// query against the Issues API; covers issues AND PRs via
    /// `is:pr`), or `actions` (workflow runs for a single `repo`).
    #[serde(default = "default_kind")]
    pub kind: String,
    /// GitHub Issues-search query. Required for `kind = "issues"`,
    /// ignored otherwise. Same syntax as the web UI search box.
    /// Reference: https://docs.github.com/en/search-github/searching-on-github/searching-issues-and-pull-requests
    #[serde(default)]
    pub query: Option<String>,
    /// `owner/name` slug. Required for `kind = "actions"`. Ignored
    /// for `kind = "issues"`.
    #[serde(default)]
    pub repo: Option<String>,
    /// Optional branch filter for `kind = "actions"`. None ⇒ all
    /// branches (most-recently-updated first).
    #[serde(default)]
    pub branch: Option<String>,
}

fn default_kind() -> String {
    "issues".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-forge-github config. Edit and re-run.

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switched via 1-9 keys (or click)
# and ordered left→right.
#
# `kind` defaults to `issues`. Supported kinds:
#   issues   — search the Issues API with `query`. The Issues API
#              also returns PRs (rows are badged via the
#              `is_pr` flag), so PR tabs use this kind too — just
#              filter with `is:pr` in the query.
#   actions  — workflow runs for a single `repo`. Optional `branch`
#              filter narrows to one branch.

# Issues / PRs assigned to you.
[[tabs]]
name = "Mine"
query = "is:open assignee:@me"

# PRs you authored across the family.
[[tabs]]
name = "My PRs"
query = "is:open is:pr author:@me"

# PRs you're reviewing.
[[tabs]]
name = "Reviewing"
query = "is:open is:pr review-requested:@me"

# Repo-scoped issue tab — swap in a repo you care about.
[[tabs]]
name = "mnml bugs"
query = "repo:chris-mclennan/mnml is:open is:issue label:bug"

# Workflow runs for a single repo (no branch filter ⇒ all branches).
# [[tabs]]
# name = "mnml CI"
# kind = "actions"
# repo = "chris-mclennan/mnml"

# Branch-filtered Actions tab.
# [[tabs]]
# name = "main CI"
# kind = "actions"
# repo = "chris-mclennan/mnml"
# branch = "main"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let valid_kind = matches!(t.kind.as_str(), "issues" | "actions");
            if !valid_kind {
                return Err(anyhow!(
                    "tab #{i} ({}): kind must be `issues` or `actions`, got `{}`",
                    t.name,
                    t.kind
                ));
            }
            match t.kind.as_str() {
                "issues" => {
                    let q = t.query.as_deref().unwrap_or("").trim();
                    if q.is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `query` is required for kind `issues`",
                            t.name
                        ));
                    }
                }
                "actions" => {
                    let r = t.repo.as_deref().unwrap_or("").trim();
                    if r.is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `repo` is required for kind `actions`",
                            t.name
                        ));
                    }
                    if !r.contains('/') {
                        return Err(anyhow!(
                            "tab #{i} ({}): `repo` must be `owner/name`, got `{r}`",
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
        .join("mnml-forge-github.toml")
}

/// #1091 (2026-08-20) — distinguish "config missing → template
/// scaffolded, please edit" from a real load error. Prior version
/// returned `Err(anyhow!("wrote config template …"))` which surfaced
/// via `main`'s `?` as `Error: wrote config template …`, making a
/// clean first-run look like a hard failure. Now `main` matches on
/// this outcome and prints a friendly info line + exit(0).
pub enum LoadOutcome {
    Loaded(Config),
    TemplateScaffolded(PathBuf),
}

pub fn load() -> Result<LoadOutcome> {
    let path = config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        return Ok(LoadOutcome::TemplateScaffolded(path));
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(LoadOutcome::Loaded(cfg))
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
    fn validate_rejects_issues_tab_with_empty_query() {
        let raw = r##"
[[tabs]]
name = "Bad"
query = ""
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_issues_tab_with_no_query() {
        let raw = r##"
[[tabs]]
name = "Bad"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
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
    fn validate_accepts_actions_kind_with_repo() {
        let raw = r##"
[[tabs]]
name = "CI"
kind = "actions"
repo = "owner/name"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_actions_without_repo() {
        let raw = r##"
[[tabs]]
name = "CI"
kind = "actions"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("repo"));
    }

    #[test]
    fn validate_rejects_actions_repo_without_slash() {
        let raw = r##"
[[tabs]]
name = "CI"
kind = "actions"
repo = "broken"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("owner/name"));
    }

    #[test]
    fn validate_rejects_unknown_kind() {
        let raw = r##"
[[tabs]]
name = "Bad"
kind = "garbage"
query = "x"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("issues"));
    }

    #[test]
    fn actions_kind_with_optional_branch_filter() {
        let raw = r##"
[[tabs]]
name = "CI main"
kind = "actions"
repo = "owner/name"
branch = "main"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.tabs[0].branch.as_deref(), Some("main"));
    }
}
