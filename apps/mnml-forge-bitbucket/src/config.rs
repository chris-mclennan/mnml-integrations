//! Config file at `~/.config/mnml-forge-bitbucket.toml`. First run
//! writes the scaffold + exits with instructions.
//!
//! tree-redesign 2026-07-14 — added three workspace-wide tab kinds
//! (`workspace_open_prs`, `workspace_merged_prs`, `workspace_pipelines`)
//! that surface every repo in the workspace at once, plus a shared
//! `Scope` + `hidden_repos` + `repo_order` shape (mirroring
//! mnml-aws-amplify's `hidden_app_ids` / `app_order`). The pre-redesign
//! `pull_requests` / `pipelines` / `branches` kinds are preserved
//! unchanged so existing user configs keep working.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Atlassian / Bitbucket Cloud account email (used as the
    /// username half of HTTP Basic auth with the app password).
    pub email: String,
    /// Default workspace slug — the part before the `/` in
    /// `bitbucket.org/<workspace>/<repo>`. Tabs can override this
    /// per-row via `workspace = "..."`.
    pub workspace: String,
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<Tab>,

    // ── shared scope shape (mirrors mnml-aws-amplify) ────────────────
    /// Which repos to include in workspace-wide tabs. `"all"` (default)
    /// enumerates every repo in the workspace; `"recent"` filters to
    /// repos with any activity in the last `recent_window_days`;
    /// `"explicit"` uses ONLY the `explicit_repos` allow-list below.
    /// Toggle at runtime with `A` (all) / `R` (recent) / `E` (explicit).
    #[serde(default = "default_scope")]
    pub scope: String,
    /// How many days back to look for "recent" activity — any commit
    /// push, PR open/update, pipeline run, or default-branch merge in
    /// this window makes the repo count as active. Only meaningful
    /// when `scope = "recent"`.
    #[serde(default = "default_recent_window")]
    pub recent_window_days: u32,
    /// Explicit allow-list of repo slugs (used when `scope = "explicit"`).
    /// Ignored for `"all"` and `"recent"`.
    #[serde(default)]
    pub explicit_repos: Vec<String>,
    /// Repo slugs to hide from every workspace-wide tab. Applied
    /// after the scope filter (so hiding always subtracts). Matches
    /// mnml-aws-amplify's `hidden_app_ids`. Runtime `x` on a repo
    /// row appends to this list + persists via `save`.
    #[serde(default)]
    pub hidden_repos: Vec<String>,
    /// Repo slugs in preferred display order. Repos listed here
    /// render first in that order; anything else follows in the
    /// API's default (usually `-updated_on`). Alt-↑ / Alt-↓ on a
    /// tree row rewrites this + persists. Matches
    /// mnml-aws-amplify's `app_order`.
    #[serde(default)]
    pub repo_order: Vec<String>,

    // ── statusline chip scoping (2026-08-17, task #996) ─────────────
    // The chip's "open PRs authored by you" count was showing 160+ for
    // users with long histories — Bitbucket honestly returns every OPEN
    // PR the account ever authored, including years-old zombies that
    // never got closed. These knobs narrow to "PRs I actually need to
    // think about" — matches Chris's Slack "Open PRs · N unapproved"
    // semantic.
    /// Ignore PRs whose `updated_on` is older than this many days when
    /// counting for the statusline chip. Default 30 = last month's
    /// activity. `0` disables (count all OPEN, regardless of age).
    #[serde(default = "default_chip_stale_after_days")]
    pub chip_stale_after_days: u32,
    /// Regex patterns matching branch names to EXCLUDE from the chip
    /// count (matched against `source.branch.name`). Default excludes
    /// release/hotfix branches — those are release-train PRs, not
    /// day-to-day work. Empty list = no branch exclusion.
    #[serde(default = "default_chip_excluded_branch_patterns")]
    pub chip_excluded_branch_patterns: Vec<String>,
    /// #1028 (2026-08-18) — integration-level repo allowlist. When
    /// non-empty, BOTH the chip poll (`--values`) AND the workspace
    /// pane tabs (`workspace_open_prs`, `workspace_merged_prs`,
    /// `workspace_pipelines`) query ONLY these repos instead of
    /// enumerating every workspace repo. Cuts what was 120 API
    /// calls / 5 min to N. Prevents the 429 storm on large
    /// workspaces (tattledevs = 119 repos). Empty (default)
    /// preserves backward-compat: fall back to `scope` /
    /// `explicit_repos` for the tabs, enumerate all for the chip.
    /// #1031 (2026-08-18) extended the honor list to the workspace
    /// tabs.
    #[serde(default)]
    pub repos: Vec<String>,
}

fn default_chip_stale_after_days() -> u32 {
    // #1078 (2026-08-20) — bumped 30 → 90. 30 was too aggressive:
    // Chris's most-recent authored OPEN PR on tattledevs is 86 days
    // old (long-running feature branch), so the chip was showing
    // `0(0)` despite there being 5+ real open PRs to surface. 90d
    // catches "still an active review target" for slower cadences
    // without regressing to the 160+ zombie count #996 originally
    // fixed. Users on hyper-active workspaces can shrink via
    // `chip_stale_after_days` in the sibling config.
    90
}

fn default_chip_excluded_branch_patterns() -> Vec<String> {
    vec!["^release/".to_string(), "^hotfix/".to_string()]
}

fn default_refresh() -> u64 {
    60
}

fn default_scope() -> String {
    // tree-redesign 2026-07-15 user report — default of "all" made
    // the Pipelines tree overwhelming (119 repos for a large
    // workspace). "recent" is a much saner default that surfaces
    // just the repos touched in the last 30 days; user can `A` at
    // runtime to switch to "all" when they want the full list.
    "recent".to_string()
}

fn default_recent_window() -> u32 {
    // tree-redesign 2026-07-15 user request — was 30, but 30 days
    // still surfaced ~30 repos in a large workspace (tattledevs).
    // 14 days catches a real sprint's worth of activity and is a
    // sharper "what's actually alive right now" signal.
    14
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// What kind of view this tab shows.
    ///
    /// **Workspace-wide (new, tree-redesign 2026-07-14):**
    ///   * `workspace_open_prs`     — every OPEN + DRAFT PR across the
    ///     workspace, all authors, newest first. Filtered through
    ///     `scope` + `hidden_repos`.
    ///   * `workspace_merged_prs`   — every MERGED PR across the
    ///     workspace, all authors, newest → oldest. Same scope /
    ///     hide filters.
    ///   * `workspace_pipelines`    — repo tree, one row per repo,
    ///     expandable to show each branch's latest pipeline status
    ///     (mnml-aws-amplify style). Uses `scope` / `hidden_repos` /
    ///     `repo_order`.
    ///
    /// **Legacy (pre-redesign, still supported):**
    ///   * `pull_requests` — per-repo or `mode = mine|reviewing` PR list
    ///   * `pipelines`     — recent builds for a single `repo`
    ///   * `branches`      — branches in a single `repo`
    ///
    /// PR-specific fields (`state`, `mode`, `q`) are ignored on
    /// non-`pull_requests` legacy kinds AND on all new workspace-wide
    /// kinds (they roll their own state).
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Override the default workspace for this tab.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Repository slug (the part after `<workspace>/`). Required for
    /// per-repo legacy PR tabs and for `pipelines` / `branches`
    /// (those endpoints don't have a workspace-spanning variant).
    /// Ignored by workspace-wide `workspace_*` kinds and by
    /// `pull_requests` with `mode = "mine"` / `"reviewing"`.
    #[serde(default)]
    pub repo: Option<String>,
    /// PR state filter — `OPEN` (default), `MERGED`, `DECLINED`,
    /// `SUPERSEDED`. Ignored on non-`pull_requests` kinds.
    #[serde(default = "default_state")]
    pub state: String,
    /// Optional mode for cross-repo PR tabs (legacy `pull_requests`):
    ///   - omitted ⇒ literal per-repo lookup (needs `repo`)
    ///   - `mine` ⇒ PRs you opened, across the workspace
    ///   - `reviewing` ⇒ PRs where you are a reviewer
    ///
    /// Both auto-modes use Bitbucket's `q=` BBQL via the workspace
    /// PR endpoint and resolve the current user's `account_id` at
    /// load time via `/2.0/user`. Ignored on non-`pull_requests` kinds.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional raw BBQL appended to the auto-mode query (or used
    /// as the only filter when `mode` and `repo` are both unset).
    /// Example: `state = "OPEN" AND author.account_id = "{abc}"`.
    /// Ignored on non-`pull_requests` kinds.
    #[serde(default)]
    pub q: Option<String>,
    /// #1099 f/u (2026-08-20) — post-fetch filter on
    /// `workspace_open_prs` / `workspace_merged_prs` that keeps only
    /// PRs authored by the auth user. Preserves the tree grouping
    /// (by repo, expandable) instead of dropping to the flat
    /// `pull_requests` mode=mine view. Not persisted from config
    /// today — set at runtime by `--only prs-mine` when no
    /// mine-mode tab exists in cfg. Ignored on non-workspace kinds.
    #[serde(default)]
    pub mine_only: bool,
}

fn default_state() -> String {
    "OPEN".to_string()
}

fn default_kind() -> String {
    "pull_requests".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-forge-bitbucket config. Edit and re-run.
#
# Required:
#   email        — your Atlassian / Bitbucket account email
#   workspace    — default workspace slug (bitbucket.org/<workspace>/<repo>)

email     = "you@example.com"
workspace = "your-workspace-slug"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Scope (workspace-wide tabs only) ─────────────────────────────
# Which repos count as "the workspace" for workspace_open_prs /
# workspace_merged_prs / workspace_pipelines. Runtime toggle:
#   A → scope="all"      (every repo)
#   R → scope="recent"   (touched in last recent_window_days)
#   E → scope="explicit" (only the explicit_repos allow-list)
scope              = "recent"
recent_window_days = 14
# explicit_repos   = ["frontend", "backend"]  # used when scope="explicit"

# Repos never surfaced (subtracts from any scope). Runtime `x` on a
# row appends here; `H` clears it.
# hidden_repos = ["archived-legacy", "stale-experiment"]

# Preferred display order — repos here render first, in this order.
# Alt-↑ / Alt-↓ on a tree row rewrites this.
# repo_order   = ["frontend", "backend", "shared-libs"]

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switched via 1-9 (or click) and
# rendered left→right.
#
# Recommended 3-tab layout (tree-redesign 2026-07-14):
#
#   workspace_open_prs      — every open + draft PR in the workspace
#   workspace_merged_prs    — every merged PR, newest first
#   workspace_pipelines     — repo tree with per-branch pipeline status

[[tabs]]
name = "Open + Draft"
kind = "workspace_open_prs"

[[tabs]]
name = "Merged"
kind = "workspace_merged_prs"

[[tabs]]
name = "Pipelines"
kind = "workspace_pipelines"

# ── Legacy per-repo tabs (still supported) ───────────────────────
#
# [[tabs]]
# name = "your-repo PRs"
# repo = "your-repo"
# state = "OPEN"
#
# [[tabs]]
# name = "your-repo pipelines"
# kind = "pipelines"
# repo = "your-repo"
"##;

    /// Legal `Tab.kind` values. Kept as a slice so validate + the doc
    /// comment can't drift.
    pub const VALID_KINDS: &'static [&'static str] = &[
        // Workspace-wide (new).
        "workspace_open_prs",
        "workspace_merged_prs",
        "workspace_pipelines",
        // Legacy per-repo.
        "pull_requests",
        "pipelines",
        "branches",
    ];

    /// Legal `scope` values.
    pub const VALID_SCOPES: &'static [&'static str] = &["all", "recent", "explicit"];

    pub fn validate(&self) -> Result<()> {
        if self.email.trim().is_empty() {
            return Err(anyhow!("config: `email` is required"));
        }
        if self.workspace.trim().is_empty() {
            return Err(anyhow!("config: `workspace` is required"));
        }
        if !Self::VALID_SCOPES.contains(&self.scope.as_str()) {
            return Err(anyhow!(
                "config: `scope` must be one of {:?}, got `{}`",
                Self::VALID_SCOPES,
                self.scope
            ));
        }
        if self.scope == "explicit" && self.explicit_repos.is_empty() {
            return Err(anyhow!(
                "config: `scope = \"explicit\"` requires a non-empty `explicit_repos` list"
            ));
        }
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
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
                // Workspace-wide kinds ignore repo/state/mode/q on the
                // tab itself — they derive their scope from top-level
                // config fields (scope, hidden_repos, repo_order).
                "workspace_open_prs" | "workspace_merged_prs" | "workspace_pipelines" => {}
                "pull_requests" => {
                    let valid_state = matches!(
                        t.state.as_str(),
                        "OPEN" | "MERGED" | "DECLINED" | "SUPERSEDED"
                    );
                    if !valid_state {
                        return Err(anyhow!(
                            "tab #{i} ({}): state must be OPEN / MERGED / DECLINED / SUPERSEDED, got `{}`",
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
                    } else if t.repo.is_none() && t.q.is_none() {
                        return Err(anyhow!(
                            "tab #{i} ({}): one of `mode`, `repo`, or `q` is required for `pull_requests`",
                            t.name
                        ));
                    }
                }
                "pipelines" | "branches" => {
                    if t.repo.is_none() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `repo` is required for kind `{}`",
                            t.name,
                            t.kind
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
        .join("mnml-forge-bitbucket.toml")
}

/// Persist the current config (used by runtime `x` hide, `H` unhide,
/// Alt-↑/↓ reorder, and `A`/`R`/`E` scope toggles). Full rewrite —
/// hand-authored comments in the file are dropped.
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
            "wrote config template to {} — edit it (set email + workspace), then re-run",
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
        // The example uses placeholder email/workspace; substitute
        // valid ones before asserting validate() passes.
        let mut cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.email = "alice@example.com".into();
        cfg.workspace = "acme".into();
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
                "workspace_pipelines"
            ]
        );
    }

    #[test]
    fn validate_rejects_missing_email() {
        let raw = r##"
email = ""
workspace = "ws"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_workspace() {
        let raw = r##"
email = "a@b.com"
workspace = ""
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_state() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Bad"
mode = "mine"
state = "PENDING"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_pipelines_kind_with_repo() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Pipelines"
kind = "pipelines"
repo = "myrepo"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_pipelines_without_repo() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Pipelines"
kind = "pipelines"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("repo"));
    }

    #[test]
    fn validate_rejects_unknown_kind() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Bad"
kind = "garbage"
repo = "myrepo"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("kind"));
    }

    #[test]
    fn validate_rejects_bad_mode() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Bad"
mode = "haha"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_tab_with_no_mode_repo_or_q() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Bad"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_no_tabs() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    // ── tree-redesign 2026-07-14 additions ──────────────────────

    #[test]
    fn validate_accepts_workspace_wide_kinds_without_repo() {
        // The new workspace_* kinds derive scope from top-level
        // config, not per-tab `repo` — validate() must allow them
        // to omit `repo` / `mode` / `q`.
        for kind in [
            "workspace_open_prs",
            "workspace_merged_prs",
            "workspace_pipelines",
        ] {
            let raw = format!(
                r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "{kind}"
kind = "{kind}"
"##
            );
            let cfg: Config = toml::from_str(&raw).unwrap();
            cfg.validate()
                .unwrap_or_else(|e| panic!("{kind} should validate: {e}"));
        }
    }

    #[test]
    fn default_scope_is_recent() {
        // tree-redesign 2026-07-15 — flipped from "all" to "recent"
        // after user hit a 119-repo Pipelines tree. Recent (30-day
        // window) is a much saner default; user presses `A` at
        // runtime to switch to all.
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scope, "recent");
        assert_eq!(cfg.recent_window_days, 14);
    }

    #[test]
    fn validate_rejects_bad_scope() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
scope = "everything"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("scope"));
    }

    #[test]
    fn validate_rejects_explicit_scope_with_no_repos() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
scope = "explicit"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("explicit_repos"));
    }

    #[test]
    fn validate_accepts_recent_scope_with_default_window() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
scope = "recent"
[[tabs]]
name = "Open"
kind = "workspace_open_prs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.recent_window_days, 14);
    }

    #[test]
    fn hidden_repos_defaults_to_empty() {
        let raw = r##"
email = "a@b.com"
workspace = "ws"
[[tabs]]
name = "Mine"
mode = "mine"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.hidden_repos.is_empty());
        assert!(cfg.repo_order.is_empty());
    }
}
