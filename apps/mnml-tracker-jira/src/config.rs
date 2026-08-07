//! TOML config — read from `~/.config/mnml-tracker-jira.toml`.
//!
//! See `Config::EXAMPLE` for the default template that gets written
//! when no file exists. Each tab is either:
//!   - a literal `jql = "..."` query, or
//!   - a `mode = "current_release" | "next_release"` (optionally
//!     scoped by `project` / `component`) that gets resolved at
//!     startup against Jira's release list.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub jira_url: String,
    pub email: String,
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    pub tabs: Vec<Tab>,
    /// 2026-07-25 — global "release branch has been cut" flag.
    /// Toggled via the `--release-cut` CLI flag or `m` keychord
    /// on a fix_version_tree tab. Feeds `BumpRules::release_cut`
    /// on Fix Versions tabs — when true, Done tickets bump to
    /// the top ("did our merges land safely on the release
    /// branch?"). Persisted to config file when toggled.
    #[serde(default)]
    pub release_cut: bool,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// 2026-07-25 — new schema. `kind = "work_assigned" | ...` picks
    /// one of the split-app tab families. When set, the legacy
    /// `jql` / `mode` fields become optional overrides (a kind
    /// implies a default JQL/resolve). When absent, we fall back
    /// to legacy behavior — an old config still works untouched.
    #[serde(default)]
    pub kind: Option<TabKind>,
    /// Legacy: auto-resolve a fixVersion. Still works standalone
    /// (kind unset) OR as an override when kind = "fix_version_tree".
    #[serde(default)]
    pub mode: Option<ResolveMode>,
    /// Legacy: literal JQL query. Still works standalone (kind
    /// unset) OR as an override when kind is set (overrides the
    /// kind's default JQL).
    #[serde(default)]
    pub jql: Option<String>,
    /// Project key (e.g. "TE"). Required when `mode` is set OR
    /// when `kind` is a fix_version / board kind.
    #[serde(default)]
    pub project: Option<String>,
    /// Component filter (e.g. "Mobile"). Optional when `mode` is set.
    #[serde(default)]
    pub component: Option<String>,
    /// Override the default column set for this tab. Useful when one
    /// tab is "Mine" and you want to see priority + reporter, while
    /// another tab is a release-tracking view where assignee + updated
    /// matter more. `None` ⇒ use the family default (key, status,
    /// assignee, updated, summary).
    #[serde(default)]
    pub columns: Option<Vec<Column>>,
    /// 2026-07-25 — Fix Versions: status group ordering. Top-to-
    /// bottom = highest-to-lowest priority. Any status missing from
    /// this list drops to the end in Jira's default alpha order.
    /// Applies only when `kind = "fix_version_tree"`; ignored on
    /// other kinds. `None` = use the built-in default.
    #[serde(default)]
    pub status_order: Option<Vec<String>>,
    /// 2026-07-25 — Fix Versions conditional bumps. Applied AFTER
    /// `status_order` groups tickets. Each rule promotes matching
    /// tickets into a different status group. Ignored on non-
    /// fix_version_tree kinds.
    #[serde(default)]
    pub bumps: Option<BumpRules>,
    /// 2026-07-26 — Fix Versions: restrict which unreleased
    /// versions the `mode = current_release / next_release`
    /// resolver considers. Substring match (case-insensitive) on
    /// version name. Useful when a project has multiple parallel
    /// release tracks (e.g. Tattle has "Mobile - 1.6.X", "N/A",
    /// "13.15.0", "IE.3.5.0") — set `version_name_contains = "."`
    /// or `= "13."` to constrain to a single track. Ignored on
    /// non-fix_version_tree kinds.
    #[serde(default)]
    pub version_name_contains: Option<String>,
    /// 2026-08-06 — Board tabs: filter kanban issues by team. A
    /// case-insensitive substring match against each issue's
    /// components AND labels — either hit keeps the ticket. Tattle
    /// splits teams via `component=web-team`/`mobile-team`/etc AND
    /// via `label=team:web`, so this handles both conventions.
    /// Ignored on non-board kinds.
    #[serde(default)]
    pub team: Option<String>,
}

/// 2026-07-25 — new TabKind enum. Powers the split rail chips
/// (`Jira Work` / `Jira Fix Versions` / `Jira Boards`). Each kind
/// implies a default JQL/resolve; the legacy `jql` / `mode` fields
/// override that default when set on the same [[tabs]] entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TabKind {
    /// Jira Work: tickets assigned to the auth user (default mode).
    WorkAssigned,
    /// Jira Work: recently transitioned to Done / Closed (last 30d).
    WorkRecentlyDone,
    /// Jira Fix Versions: expandable tree grouped by status, one
    /// row per ticket. `project` required. Default = current
    /// unreleased fixVersion; override with `mode = "next_release"`
    /// or a fully custom `jql`.
    FixVersionTree,
    /// Jira Boards: active sprint of the project's default board.
    /// `project` required.
    BoardActiveSprint,
    /// Jira Boards: backlog (tickets NOT in an active sprint).
    BoardBacklog,
}

impl TabKind {
    /// 2026-07-25 — which of the split chips (`--only` values) a
    /// tab belongs to. Used to filter cfg.tabs when the CLI passes
    /// `--only <family>`.
    pub fn family(self) -> TabFamily {
        match self {
            Self::WorkAssigned | Self::WorkRecentlyDone => TabFamily::Work,
            Self::FixVersionTree => TabFamily::FixVersions,
            Self::BoardActiveSprint | Self::BoardBacklog => TabFamily::Boards,
        }
    }

    /// Default JQL for the kind when the user hasn't supplied a
    /// custom `jql`. Fix-version kinds return `None` because their
    /// JQL is built dynamically (depends on the resolved fixVersion
    /// name — see `ResolveMode` handling in app.rs).
    pub fn default_jql(self) -> Option<&'static str> {
        match self {
            Self::WorkAssigned => Some(
                "assignee = currentUser() \
                 AND resolution = Unresolved \
                 ORDER BY updated DESC",
            ),
            Self::WorkRecentlyDone => Some(
                "assignee = currentUser() \
                 AND status in (Done, Closed, Resolved) \
                 AND resolved >= -30d \
                 ORDER BY resolved DESC",
            ),
            Self::FixVersionTree => None,
            Self::BoardActiveSprint => Some("sprint in openSprints() ORDER BY rank ASC"),
            Self::BoardBacklog => Some(
                "sprint is EMPTY \
                 AND status != Done \
                 ORDER BY rank ASC",
            ),
        }
    }
}

/// 2026-07-25 — split-chip family. Matches the CLI `--only`
/// values (`work` / `fix-versions` / `boards`) so the launcher
/// chip can filter cfg.tabs down to a single family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabFamily {
    Work,
    FixVersions,
    Boards,
}

impl TabFamily {
    /// Parse the CLI `--only <value>`. Returns `None` on unknown.
    pub fn from_cli(s: &str) -> Option<Self> {
        match s {
            "work" | "jira_work" => Some(Self::Work),
            "fix-versions" | "fix_versions" | "fix_version" => Some(Self::FixVersions),
            "boards" | "jira_boards" => Some(Self::Boards),
            _ => None,
        }
    }
}

/// 2026-07-25 — Fix Versions conditional bump rules. Each field
/// names a STATUS to promote matching tickets INTO. `None` on a
/// field ⇒ that rule is disabled.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BumpRules {
    /// Tickets in "PR Review"-ish statuses that have ≥1 approving
    /// reviewer on their linked PRs get promoted to the named
    /// status group. Typical value: `"Testing"`.
    #[serde(default)]
    pub pr_approved: Option<String>,
    /// Tickets in "PR Review"-ish statuses that have zero OPEN or
    /// DRAFT PRs left get promoted. Signal: dev forgot to move the
    /// ticket to Testing after merging. Typical: `"Testing"`.
    #[serde(default)]
    pub no_open_prs: Option<String>,
    /// When the release branch has been cut (see `Config::release_cut`
    /// or `--release-cut` flag), promote matching statuses. Keys are
    /// status names, values are the target status group OR the
    /// special string `"top"` to force to the very top.
    #[serde(default)]
    pub release_cut: std::collections::HashMap<String, String>,
}

/// One column in the issue table. Used in per-tab overrides via
/// `[[tabs]] columns = [...]`. Case is preserved on input; serde
/// expects snake-case strings (`"fix_version"`, etc.).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Key,
    Status,
    Assignee,
    Reporter,
    Priority,
    Type,
    Updated,
    FixVersion,
    Summary,
}

impl Column {
    /// The family default — what every tab gets when `columns` is unset.
    pub fn default_set() -> Vec<Column> {
        vec![
            Column::Key,
            Column::Status,
            Column::Assignee,
            Column::Updated,
            Column::Summary,
        ]
    }

    /// Header label for the column.
    pub fn header(self) -> &'static str {
        match self {
            Column::Key => "KEY",
            Column::Status => "STATUS",
            Column::Assignee => "ASSIGNEE",
            Column::Reporter => "REPORTER",
            Column::Priority => "PRIORITY",
            Column::Type => "TYPE",
            Column::Updated => "UPDATED",
            Column::FixVersion => "FIXVERSION",
            Column::Summary => "SUMMARY",
        }
    }

    /// Render width (in cells) — `None` ⇒ "fill remaining space"
    /// (used by Summary; only one such column makes sense per row).
    pub fn width(self) -> Option<u16> {
        match self {
            Column::Key => Some(10),
            Column::Status => Some(14),
            Column::Assignee => Some(20),
            Column::Reporter => Some(20),
            Column::Priority => Some(10),
            Column::Type => Some(10),
            Column::Updated => Some(12),
            Column::FixVersion => Some(14),
            Column::Summary => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolveMode {
    /// Earliest unreleased fixVersion of `project`.
    CurrentRelease,
    /// Second-earliest unreleased fixVersion of `project` (falls
    /// back to `CurrentRelease` if there's only one).
    NextRelease,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-tracker-jira config. Edit and re-run.

# Your Atlassian-hosted Jira instance.
jira_url = "https://yourorg.atlassian.net"

# Email associated with your API token. Used as the HTTP basic-auth
# username. Generate the token at:
#   https://id.atlassian.com/manage-profile/security/api-tokens
# and save it (chmod 600) to:
#   ~/.config/mnml-tracker-jira/token
email = "you@example.com"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# Global flag: is the release branch cut? Toggled with `m` on a
# fix_version_tree tab, or set explicitly here. Feeds the
# `release_cut` bump rules on Fix Versions tabs.
release_cut = false

# ── Tabs ─────────────────────────────────────────────────────────
# 2026-07-25: `kind = "..."` is the recommended entry point — one of
#   "work_assigned"       "work_recently_done"
#   "fix_version_tree"
#   "board_active_sprint" "board_backlog"
# Each kind implies a default JQL; `jql = "..."` overrides it.
# Legacy configs (no `kind`, just `jql` or `mode`) still work.
#
# The launcher chips filter these tabs via `--only <family>`:
#   Jira Work         → work_assigned, work_recently_done
#   Jira Fix Versions → fix_version_tree
#   Jira Boards       → board_active_sprint, board_backlog

# Jira Work — assigned to me (default when the chip opens).
[[tabs]]
name = "Assigned"
kind = "work_assigned"

# Jira Work — my last 30d of Done tickets. Toggle with `m`.
[[tabs]]
name = "Recently Done"
kind = "work_recently_done"

# Jira Fix Versions — the current unreleased version, grouped by
# status. Toggle `m` to switch to next release; toggle release-cut
# context to promote Done tickets.
[[tabs]]
name = "Current Release"
kind = "fix_version_tree"
project = "TE"
mode = "current_release"
# Status priority (top-to-bottom = highest priority group).
# Missing statuses drop to Jira's alpha order at the bottom.
status_order = ["Testing", "In PR Review", "In Progress", "To Do", "Done"]

# Bump rules kick in AFTER status_order groups tickets.
[tabs.bumps]
pr_approved  = "Testing"   # PR-Review + approvals ⇒ treat as Testing
no_open_prs  = "Testing"   # PR-Review + no open PRs ⇒ dev forgot to move
# When release_cut is true (see top-level flag), Done bumps to top.
release_cut  = { Done = "top" }

# Jira Boards — active sprint of the project.
[[tabs]]
name = "Sprint"
kind = "board_active_sprint"
project = "TE"

# Jira Boards — backlog.
[[tabs]]
name = "Backlog"
kind = "board_backlog"
project = "TE"

# ── Legacy shape (still supported) ────────────────────────────────
# The pre-2026-07-25 `jql = "..."` and `mode = "..."` forms still
# work standalone — omit `kind` and set one of them.
#
# [[tabs]]
# name = "Testing"
# jql  = "status = Testing AND assignee = currentUser() ORDER BY updated DESC"
"##;

    /// 2026-07-25 — default status ordering for Fix Versions tabs
    /// when the user hasn't set `status_order`. Reflects the
    /// "what can I act on right now" priority: Testing at top,
    /// Done at bottom (until release_cut flips the game).
    pub fn default_status_order() -> Vec<String> {
        [
            "Testing",
            "In PR Review",
            "Code Review",
            "In Progress",
            "To Do",
            "Open",
            "Done",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!("tab #{i} ({})", t.name);
            // 2026-07-25 — kind branch: kind + optional jql/mode
            // overrides. When kind is set the validator only checks
            // that kind-specific requirements are met (e.g.
            // fix_version_tree needs `project`) and that
            // jql/mode aren't BOTH set.
            if let Some(kind) = t.kind {
                if t.jql.is_some() && t.mode.is_some() {
                    return Err(anyhow!(
                        "{label}: set at most one of `jql` or `mode` (kind supplies a default)"
                    ));
                }
                match kind {
                    TabKind::FixVersionTree
                    | TabKind::BoardActiveSprint
                    | TabKind::BoardBacklog => {
                        if t.project.is_none() {
                            return Err(anyhow!(
                                "{label}: kind = '{kind:?}' requires project = '<KEY>'"
                            ));
                        }
                    }
                    TabKind::WorkAssigned | TabKind::WorkRecentlyDone => {}
                }
                continue;
            }
            // Legacy branch (no `kind`): exactly one of jql/mode
            // required. Preserved so existing configs work untouched.
            match (&t.jql, &t.mode) {
                (Some(_), None) => {}
                (None, Some(_)) => {
                    if t.project.is_none() {
                        return Err(anyhow!("{label}: mode = '...' requires project = '<KEY>'"));
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(anyhow!(
                        "{label}: set exactly one of `jql` or `mode`, not both"
                    ));
                }
                (None, None) => {
                    return Err(anyhow!(
                        "{label}: set `kind = \"...\"`, `jql = \"...\"`, or `mode = \"current_release|next_release\"`"
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn default_config_path() -> PathBuf {
    // Use `~/.config/` everywhere (including macOS) — matches what
    // the README documents and what the rest of the family TUIs do,
    // rather than the OS-default `~/Library/Application Support/`.
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-tracker-jira.toml")
}

/// Load the config from `path`. If the file doesn't exist, write the
/// example template there and return an error pointing the user at it.
pub fn load_or_init(path: &std::path::Path) -> Result<Config> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, Config::EXAMPLE)
            .with_context(|| format!("writing example config to {}", path.display()))?;
        return Err(anyhow!(
            "no config found — wrote an example to {}.\n\
             Edit it (jira_url + email at minimum), generate an API token at\n\
               https://id.atlassian.com/manage-profile/security/api-tokens\n\
             and save the token (chmod 600) to:\n\
               {}",
            path.display(),
            crate::auth::token_path().display(),
        ));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config from {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&raw).with_context(|| format!("parsing config from {}", path.display()))?;
    cfg.validate()?;
    Ok(cfg)
}

/// Pretty-print the resolved config + auth hints. Used by `--check`.
pub fn print_check_report(cfg: &Config, path: &std::path::Path) -> Result<()> {
    println!("config: {}", path.display());
    println!("  jira_url: {}", cfg.jira_url);
    println!("  email:    {}", cfg.email);
    println!("  refresh:  {}s", cfg.refresh_interval_secs);
    println!("  tabs:     {}", cfg.tabs.len());
    for (i, t) in cfg.tabs.iter().enumerate() {
        let kind = if let Some(m) = &t.mode {
            format!(
                "{:?}{}{}",
                m,
                t.project
                    .as_deref()
                    .map(|p| format!(" project={p}"))
                    .unwrap_or_default(),
                t.component
                    .as_deref()
                    .map(|c| format!(" component={c}"))
                    .unwrap_or_default(),
            )
        } else {
            format!("jql = {}", t.jql.as_deref().unwrap_or(""))
        };
        println!("    {}: {} → {}", i + 1, t.name, kind);
    }
    let token_path = crate::auth::token_path();
    if token_path.exists() {
        println!("token:    {} (present)", token_path.display());
    } else {
        println!("token:    {} (MISSING)", token_path.display());
        println!("  Generate at https://id.atlassian.com/manage-profile/security/api-tokens");
        println!("  Save the token to that path (chmod 600) and re-run.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert_eq!(cfg.tabs.len(), 5, "example should have 5 tabs");
        assert!(
            cfg.tabs
                .iter()
                .any(|t| t.kind == Some(TabKind::WorkAssigned))
        );
        assert!(
            cfg.tabs
                .iter()
                .any(|t| t.kind == Some(TabKind::FixVersionTree))
        );
        assert!(
            cfg.tabs
                .iter()
                .any(|t| t.kind == Some(TabKind::BoardActiveSprint))
        );
    }

    #[test]
    fn example_config_parses_bumps_on_fix_versions_tab() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).unwrap();
        let fv = cfg
            .tabs
            .iter()
            .find(|t| t.kind == Some(TabKind::FixVersionTree))
            .unwrap();
        let bumps = fv.bumps.as_ref().unwrap();
        assert_eq!(bumps.pr_approved.as_deref(), Some("Testing"));
        assert_eq!(bumps.no_open_prs.as_deref(), Some("Testing"));
        assert_eq!(
            bumps.release_cut.get("Done").map(String::as_str),
            Some("top")
        );
    }

    #[test]
    fn tab_family_split_matches_only_flag() {
        assert_eq!(TabKind::WorkAssigned.family(), TabFamily::Work);
        assert_eq!(TabKind::WorkRecentlyDone.family(), TabFamily::Work);
        assert_eq!(TabKind::FixVersionTree.family(), TabFamily::FixVersions);
        assert_eq!(TabKind::BoardActiveSprint.family(), TabFamily::Boards);
        assert_eq!(TabKind::BoardBacklog.family(), TabFamily::Boards);
        assert_eq!(TabFamily::from_cli("work"), Some(TabFamily::Work));
        assert_eq!(
            TabFamily::from_cli("fix-versions"),
            Some(TabFamily::FixVersions)
        );
        assert_eq!(TabFamily::from_cli("boards"), Some(TabFamily::Boards));
        assert_eq!(TabFamily::from_cli("bogus"), None);
    }

    #[test]
    fn work_assigned_default_jql_filters_by_current_user() {
        let jql = TabKind::WorkAssigned.default_jql().unwrap();
        assert!(jql.contains("assignee = currentUser()"));
        assert!(jql.contains("resolution = Unresolved"));
    }

    #[test]
    fn fix_version_tree_default_jql_is_none_because_dynamic() {
        // Fix-version JQL is built at runtime after resolving the
        // version name — no static default.
        assert!(TabKind::FixVersionTree.default_jql().is_none());
    }

    #[test]
    fn validate_rejects_kinded_tab_without_project_when_required() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
kind = "fix_version_tree"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_kinded_tab_with_jql_override() {
        // kind + jql override is legal (jql wins over the kind default).
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Custom"
kind = "work_assigned"
jql = "assignee = currentUser() AND priority = Highest"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().expect("kind + jql override should validate");
    }

    #[test]
    fn validate_rejects_kinded_tab_with_both_jql_and_mode_override() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
kind = "fix_version_tree"
project = "TE"
jql  = "x"
mode = "next_release"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn default_status_order_has_testing_at_top_done_at_bottom() {
        let order = Config::default_status_order();
        let testing_idx = order.iter().position(|s| s == "Testing").unwrap();
        let done_idx = order.iter().position(|s| s == "Done").unwrap();
        assert!(
            testing_idx < done_idx,
            "Testing should come before Done in the default sort"
        );
    }

    #[test]
    fn legacy_config_still_parses() {
        // Pre-2026-07-25 shape — no kind, just jql/mode. Must
        // continue to validate for backward compat.
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Testing"
jql = "status = Testing ORDER BY updated DESC"

[[tabs]]
name = "Current"
mode = "current_release"
project = "TE"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        cfg.validate().expect("legacy shape should still work");
        assert_eq!(cfg.tabs.len(), 2);
        assert!(cfg.tabs.iter().all(|t| t.kind.is_none()));
    }

    #[test]
    fn columns_default_set_is_the_five_classic_columns() {
        assert_eq!(
            Column::default_set(),
            vec![
                Column::Key,
                Column::Status,
                Column::Assignee,
                Column::Updated,
                Column::Summary,
            ]
        );
    }

    #[test]
    fn columns_summary_has_no_explicit_width() {
        assert!(Column::Summary.width().is_none());
        // Every non-summary column has a fixed width.
        for c in [
            Column::Key,
            Column::Status,
            Column::Assignee,
            Column::Reporter,
            Column::Priority,
            Column::Type,
            Column::Updated,
            Column::FixVersion,
        ] {
            assert!(c.width().is_some(), "{c:?} should have a fixed width");
        }
    }

    #[test]
    fn columns_snake_case_serde_round_trip() {
        let toml_in = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Demo"
jql = "x"
columns = ["fix_version", "summary"]
"##;
        let cfg: Config = toml::from_str(toml_in).unwrap();
        assert_eq!(
            cfg.tabs[0].columns,
            Some(vec![Column::FixVersion, Column::Summary])
        );
    }

    #[test]
    fn validate_rejects_tab_with_both_jql_and_mode() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
jql = "status = Open"
mode = "current_release"
project = "X"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_tab_with_neither_jql_nor_mode() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_mode_tab_missing_project() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
mode = "current_release"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }
}
