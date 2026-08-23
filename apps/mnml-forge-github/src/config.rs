//! Config file at `~/.config/mnml-forge-github.toml`. First run
//! writes the scaffold + exits with instructions.
//!
//! workspace-tabs 2026-08-22 (task #1092) — mirrors the design from
//! mnml-forge-bitbucket 0.3.29 (task #1031). Three new workspace-wide
//! tab kinds surface PRs / Actions across every repo owned by an
//! `owner` (a GitHub user or org). Legacy `issues` / `actions` per-tab
//! kinds are preserved so pre-0.3 configs keep working.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,

    /// Default owner (a GitHub org OR user login) for the
    /// `workspace_*` tabs. Empty ⇒ those tabs error at load time
    /// unless the tab itself sets `owner = "..."`. This is the
    /// closest GitHub analog to Bitbucket's `workspace` slug.
    #[serde(default)]
    pub owner: String,

    /// Tab list — at least one required. `#[serde(default)]` lets a
    /// config-with-no-tabs parse so `validate()` can produce a
    /// human-readable error rather than the cryptic
    /// `missing field "tabs"` from serde.
    #[serde(default)]
    pub tabs: Vec<Tab>,

    // ── shared scope shape (mirrors mnml-forge-bitbucket) ───────────
    /// Which repos to include in workspace-wide tabs. `"all"`
    /// enumerates every repo owned by `owner`; `"recent"` filters to
    /// repos with any push activity in the last `recent_window_days`;
    /// `"explicit"` uses ONLY the `explicit_repos` allow-list below.
    /// Toggle at runtime with `s` (all → recent → explicit → all).
    #[serde(default = "default_scope")]
    pub scope: String,
    /// How many days back to look for "recent" activity — a repo's
    /// `pushed_at` inside this window counts as active. Only
    /// meaningful when `scope = "recent"`.
    #[serde(default = "default_recent_window")]
    pub recent_window_days: u32,
    /// Explicit allow-list of `owner/repo` slugs, or bare `repo`
    /// names (which resolve against the default `owner`). Used only
    /// when `scope = "explicit"`.
    #[serde(default)]
    pub explicit_repos: Vec<String>,
    /// Repo slugs to hide from every workspace-wide tab. Applied
    /// after the scope filter (so hiding always subtracts). Runtime
    /// `x` on a row appends to this list + persists via `save`.
    #[serde(default)]
    pub hidden_repos: Vec<String>,
    /// Repo slugs in preferred display order. Repos listed here
    /// render first in that order; anything else follows in the
    /// API's default (usually `-pushed_at`). Alt-↑ / Alt-↓ on a
    /// tree row rewrites this + persists.
    #[serde(default)]
    pub repo_order: Vec<String>,

    /// #1092 (2026-08-22) — integration-level repo allowlist,
    /// analog of mnml-forge-bitbucket's `repos` field (#1028/#1031).
    /// When non-empty, the workspace tabs query ONLY these repos
    /// instead of enumerating everything owned by `owner`. Cuts
    /// fan-out for users on very large orgs. Empty (default)
    /// preserves the enumerate-then-filter behavior via `scope`.
    /// Slugs may be bare `repo` (resolved against `owner`) or
    /// fully-qualified `owner/repo` (useful for pulling in a repo
    /// from a different owner without splitting into a new tab).
    #[serde(default)]
    pub repos: Vec<String>,
}

fn default_refresh() -> u64 {
    60
}

fn default_scope() -> String {
    "recent".to_string()
}

fn default_recent_window() -> u32 {
    // Matches mnml-forge-bitbucket's #1079 tuning — 14 days catches
    // a real sprint's worth of activity without swamping the tree.
    14
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// What kind of view this tab shows.
    ///
    /// **Workspace-wide (workspace-tabs 2026-08-22):**
    ///   * `workspace_open_prs`     — every OPEN PR (incl. draft)
    ///     across the owner's repos, newest first, grouped by repo.
    ///     Filtered through `scope` + `hidden_repos` / `repos`.
    ///   * `workspace_merged_prs`   — every MERGED (closed with
    ///     merge) PR across the owner's repos, newest first. Same
    ///     scope / hide filters.
    ///   * `workspace_actions`      — repo tree, one row per repo,
    ///     expandable to show recent Actions workflow runs.
    ///
    /// **Legacy (pre-workspace-tabs):**
    ///   * `issues`  — search the Issues API with `query`. Covers
    ///     issues + PRs (badged via `is:pr` in the query).
    ///   * `actions` — Actions workflow runs for a single `repo`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Optional owner override for this tab. Ignored on `issues`
    /// (queries carry their own scoping). Used by `actions` +
    /// every `workspace_*` kind.
    #[serde(default)]
    pub owner: Option<String>,
    /// GitHub Issues-search query. Required for `kind = "issues"`.
    /// Ignored otherwise. Same syntax as the web UI search box.
    /// Reference: https://docs.github.com/en/search-github/searching-on-github/searching-issues-and-pull-requests
    #[serde(default)]
    pub query: Option<String>,
    /// `owner/name` slug. Required for `kind = "actions"`. Ignored
    /// on `issues` and every workspace-wide kind (those derive
    /// scope from top-level config).
    #[serde(default)]
    pub repo: Option<String>,
    /// Optional branch filter for `kind = "actions"`. None ⇒ all
    /// branches (most-recently-updated first).
    #[serde(default)]
    pub branch: Option<String>,
    /// Post-fetch filter on `workspace_open_prs` / `workspace_merged_prs`
    /// that keeps only PRs authored by the auth user. Preserves the
    /// tree grouping (by repo, expandable) instead of dropping to a
    /// flat "search for author:@me" query. Ignored on other kinds.
    #[serde(default)]
    pub mine_only: bool,
}

fn default_kind() -> String {
    "issues".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-forge-github config. Edit and re-run.

# Default owner for the workspace_* tabs — a GitHub org or your own
# login. Individual tabs can override with `owner = "..."`.
owner = "your-github-user-or-org"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Scope (workspace-wide tabs only) ─────────────────────────────
# Which repos count as "the workspace" for workspace_open_prs /
# workspace_merged_prs / workspace_actions. Runtime toggle: `s`
# cycles all → recent → explicit → all.
scope              = "recent"
recent_window_days = 14
# explicit_repos   = ["mnml", "mnml-forge-github"]  # for scope="explicit"

# Repos never surfaced (subtracts from any scope). Runtime `x` on a
# row appends here; `H` clears it.
# hidden_repos = ["archived-legacy"]

# Preferred display order — repos here render first, in this order.
# Alt-↑ / Alt-↓ on a tree row rewrites this.
# repo_order   = ["mnml", "mnml-forge-github", "mnml-bridge"]

# Integration-level repo allowlist. When non-empty the workspace
# tabs skip the enumeration step entirely and hit only these
# repos. Slugs are bare (resolved against `owner`) or fully
# qualified as `owner/repo`.
# repos = ["mnml", "some-other-org/tool"]

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switched via 1-9 keys (or click)
# and rendered left→right.
#
# Recommended 3-tab layout (workspace-tabs 2026-08-22):

[[tabs]]
name = "Open + Draft"
kind = "workspace_open_prs"

[[tabs]]
name = "Merged"
kind = "workspace_merged_prs"

[[tabs]]
name = "Actions"
kind = "workspace_actions"

# ── Legacy per-repo tabs (still supported) ───────────────────────
#
# [[tabs]]
# name = "My PRs"
# kind = "issues"
# query = "is:open is:pr author:@me"
#
# [[tabs]]
# name = "mnml CI"
# kind = "actions"
# repo = "chris-mclennan/mnml"
"##;

    pub const VALID_KINDS: &'static [&'static str] = &[
        // Workspace-wide (workspace-tabs 2026-08-22).
        "workspace_open_prs",
        "workspace_merged_prs",
        "workspace_actions",
        // Legacy per-repo.
        "issues",
        "actions",
    ];

    pub const VALID_SCOPES: &'static [&'static str] = &["all", "recent", "explicit"];

    pub fn validate(&self) -> Result<()> {
        if !Self::VALID_SCOPES.contains(&self.scope.as_str()) {
            return Err(anyhow!(
                "config: `scope` must be one of {:?}, got `{}`",
                Self::VALID_SCOPES,
                self.scope
            ));
        }
        if self.scope == "explicit" && self.explicit_repos.is_empty() && self.repos.is_empty() {
            return Err(anyhow!(
                "config: `scope = \"explicit\"` requires a non-empty `explicit_repos` list \
                 (or a non-empty top-level `repos` allowlist)"
            ));
        }
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        // Any workspace_* tab requires either a top-level `owner`, a
        // per-tab `owner`, or a non-empty `repos` allowlist (whose
        // entries can be fully qualified `owner/repo`).
        let needs_owner = self.tabs.iter().any(|t| {
            matches!(
                t.kind.as_str(),
                "workspace_open_prs" | "workspace_merged_prs" | "workspace_actions"
            ) && t.owner.is_none()
        });
        if needs_owner && self.owner.trim().is_empty() && self.repos.is_empty() {
            return Err(anyhow!(
                "config: `owner` is required at top level when using any \
                 workspace_* tab kind (or set a per-tab `owner`, or a top-level \
                 `repos` allowlist of fully-qualified owner/repo slugs)"
            ));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            if !Self::VALID_KINDS.contains(&t.kind.as_str()) {
                return Err(anyhow!(
                    "tab #{i} ({}): kind must be one of {:?}, got `{}`",
                    t.name,
                    Self::VALID_KINDS,
                    t.kind
                ));
            }
            match t.kind.as_str() {
                // Workspace-wide kinds derive their scope from top-level
                // config; nothing per-tab to validate beyond kind.
                "workspace_open_prs" | "workspace_merged_prs" | "workspace_actions" => {}
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

/// Persist the current config (used by runtime `x` hide, `H` unhide,
/// Alt-↑/↓ reorder, and `s` scope cycle). Full rewrite — hand-authored
/// comments in the file are dropped.
pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg).map_err(|e| anyhow!("serialize config: {e}"))?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// #1091 (2026-08-20) — distinguish "config missing → template
/// scaffolded, please edit" from a real load error.
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
        let mut cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.owner = "alice".into();
        cfg.validate().expect("example validates after fill-in");
        assert!(cfg.tabs.len() >= 3);
    }

    #[test]
    fn example_config_defaults_to_the_three_workspace_wide_tabs() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("parses");
        let kinds: Vec<&str> = cfg.tabs.iter().map(|t| t.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "workspace_open_prs",
                "workspace_merged_prs",
                "workspace_actions"
            ]
        );
    }

    #[test]
    fn validate_rejects_workspace_tab_without_owner() {
        let raw = r##"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("owner"));
    }

    #[test]
    fn validate_accepts_workspace_tab_with_top_owner() {
        let raw = r##"
owner = "alice"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_accepts_workspace_tab_with_per_tab_owner() {
        let raw = r##"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
owner = "acme"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_accepts_workspace_tab_with_repos_allowlist() {
        // A fully-qualified allowlist is enough to satisfy the
        // owner requirement — the tab can dispatch per repo without
        // needing a default owner to enumerate.
        let raw = r##"
repos = ["acme/tool", "beta/lib"]
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_issues_tab_with_no_query() {
        let raw = r##"
[[tabs]]
name = "Bad"
kind = "issues"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
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
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("garbage"));
    }

    #[test]
    fn default_scope_is_recent() {
        let raw = r##"
owner = "alice"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scope, "recent");
        assert_eq!(cfg.recent_window_days, 14);
    }

    #[test]
    fn validate_rejects_bad_scope() {
        let raw = r##"
owner = "alice"
scope = "everything"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("scope"));
    }

    #[test]
    fn validate_rejects_explicit_scope_with_no_repos() {
        let raw = r##"
owner = "alice"
scope = "explicit"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("explicit_repos"));
    }

    #[test]
    fn hidden_repos_and_repo_order_default_to_empty() {
        let raw = r##"
owner = "alice"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.hidden_repos.is_empty());
        assert!(cfg.repo_order.is_empty());
        assert!(cfg.repos.is_empty());
    }
}
