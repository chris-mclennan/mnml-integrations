//! Claude-dispatch bridge.
//!
//! When the user activates an action button on a ticket (e.g.
//! `[ Implement ]` on a story), we need to hand the task off to
//! a Claude Code session. The user described two paths, both
//! implemented here (dual-write with fallback):
//!
//! 1. **Queue file** (preferred): append a JSONL line to
//!    `~/Projects/the configured dispatch_workspace/.claude/queue.jsonl`.
//!    A watcher agent in that workspace picks it up and dispatches
//!    to the appropriate `/agents:*` command. This is the "async,
//!    goes into someone's queue" path.
//!
//! 2. **Spawn Pty pane** (fallback): tell mnml (via its IPC
//!    channel at `<workspace>/.mnml/ipc/command`) to open a new
//!    Pty pane running `claude` with the prompt pre-filled. This
//!    is the "I want to see it happen now" path.
//!
//! Both are attempted on each activation — the queue write is
//! best-effort (missing queue file / no workspace = skip), and
//! the pane spawn is best-effort too (mnml not running = skip).
//! Combining them means the user gets whichever channel is
//! reachable, without needing a config toggle.

use crate::jira::Issue;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One dispatched action. Fields chosen to match what a downstream
/// workspace's `/agents:*` commands expect on stdin.
#[derive(Debug, Clone, Serialize)]
pub struct Dispatch {
    /// e.g. "implement", "fix", "triage", "review". Matches the
    /// button label lowercased (minus the brackets).
    pub kind: String,
    /// Jira issue key (e.g. "TE-14337").
    pub issue_key: String,
    /// Ticket type (e.g. "Story", "Bug", "Task"). Helps the
    /// dispatcher pick the right agent template.
    pub issue_type: String,
    /// Ticket summary — one-line title.
    pub summary: String,
    /// Direct browser URL to the Jira ticket. The agent's
    /// welcome message includes this so the user can click over
    /// if needed.
    pub jira_url: String,
    /// PR URL when the dispatch is per-PR (Review button). None
    /// for ticket-level dispatches (Implement/Fix/Triage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// ISO-8601 timestamp of the dispatch (for the queue log
    /// audit trail).
    pub queued_at: String,
}

impl Dispatch {
    /// Build a ticket-level dispatch (Implement / Fix / Triage).
    pub fn for_ticket(kind: &str, issue: &Issue, jira_url: String) -> Self {
        Self {
            kind: kind.to_string(),
            issue_key: issue.key.clone(),
            issue_type: issue
                .fields
                .issuetype
                .as_ref()
                .map(|t| t.name.clone())
                .unwrap_or_default(),
            summary: issue.fields.summary.clone(),
            jira_url,
            pr_url: None,
            queued_at: iso_now(),
        }
    }

    /// Build a PR-level dispatch (Review).
    pub fn for_pr(issue: &Issue, jira_url: String, pr_url: String) -> Self {
        Self {
            kind: "review".to_string(),
            issue_key: issue.key.clone(),
            issue_type: issue
                .fields
                .issuetype
                .as_ref()
                .map(|t| t.name.clone())
                .unwrap_or_default(),
            summary: issue.fields.summary.clone(),
            jira_url,
            pr_url: Some(pr_url),
            queued_at: iso_now(),
        }
    }

    /// Produce the prompt string that the fallback Pty pane
    /// spawns Claude with. Mirrors the JSONL payload so the
    /// agent has the same context either way.
    pub fn prompt(&self) -> String {
        let slash = match self.kind.as_str() {
            "implement" | "fix" | "triage" => format!("/agents:developer {}", self.issue_key),
            "review" => match &self.pr_url {
                Some(url) => format!("/agents:reviewer {url}"),
                None => format!("/agents:reviewer {}", self.issue_key),
            },
            "test" => format!("/agents:tester {} mode=ticket", self.issue_key),
            other => format!("/agents:{other} {}", self.issue_key),
        };
        format!(
            "{slash}\n\n<!-- context -->\n\
             kind: {kind}\n\
             ticket: {key} ({ty}) — {summary}\n\
             url: {url}\n{pr_line}",
            kind = self.kind,
            key = self.issue_key,
            ty = self.issue_type,
            summary = self.summary,
            url = self.jira_url,
            pr_line = self
                .pr_url
                .as_ref()
                .map(|u| format!("pr: {u}\n"))
                .unwrap_or_default(),
        )
    }
}

/// Dispatch to both channels. `mnml_ipc_dir` is where we look
/// for the current mnml session's `command` file (usually
/// `<workspace>/.mnml/ipc/`); pass `None` to skip the Pty spawn
/// path. `queue_dir` is where the JSONL queue lives (usually
/// `<workspace>/.claude/`); pass `None` to skip the queue write.
///
/// Returns a summary string listing which channels fired. Both
/// paths are best-effort; failures land on the summary line
/// rather than propagating.
pub fn dispatch(d: &Dispatch, queue_dir: Option<&Path>, mnml_ipc_dir: Option<&Path>) -> String {
    let mut fired: Vec<&str> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if let Some(dir) = queue_dir {
        match write_to_queue(d, dir) {
            Ok(()) => fired.push("queue"),
            Err(e) => errors.push(format!("queue: {e}")),
        }
    }
    if let Some(dir) = mnml_ipc_dir {
        match spawn_pty(d, dir) {
            Ok(()) => fired.push("pane"),
            Err(e) => errors.push(format!("pane: {e}")),
        }
    }

    match (fired.is_empty(), errors.is_empty()) {
        (false, true) => format!("{} → {}", d.kind, fired.join(" + ")),
        (false, false) => format!(
            "{} → {} (also: {})",
            d.kind,
            fired.join(" + "),
            errors.join("; ")
        ),
        (true, false) => format!("{} failed: {}", d.kind, errors.join("; ")),
        (true, true) => format!("{}: no dispatch channels available", d.kind),
    }
}

/// Append `d` as one JSON line to `<queue_dir>/queue.jsonl`.
/// Creates the parent dir if needed. Uses append mode so
/// concurrent writes from other siblings interleave cleanly at
/// the line boundary (POSIX guarantees atomicity for writes
/// under PIPE_BUF, and one Dispatch JSON line is well under 4K).
fn write_to_queue(d: &Dispatch, queue_dir: &Path) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(queue_dir)
        .with_context(|| format!("mkdir -p {}", queue_dir.display()))?;
    let path = queue_dir.join("queue.jsonl");
    let line = serde_json::to_string(d).context("serializing dispatch")?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{line}").with_context(|| format!("writing to {}", path.display()))?;
    Ok(())
}

/// Ask the running mnml (via IPC) to open a new Pty pane running
/// `claude` with the prompt pre-filled as a first user message.
/// mnml's IPC accepts `{"cmd":"term","args":[...]}` — see
/// `mnml/src/ipc/mod.rs::parse_command` for the schema. We use
/// the shell form (`sh -c`) so heredoc / echo can seed stdin.
fn spawn_pty(d: &Dispatch, ipc_dir: &Path) -> Result<()> {
    use std::io::Write;
    let cmd_path = ipc_dir.join("command");
    // Only fire if mnml appears to be running (the command file
    // exists — mnml creates it on startup). Silent no-op when
    // there's no session to receive.
    if !cmd_path.exists() {
        anyhow::bail!("no mnml IPC command file at {}", cmd_path.display());
    }
    let prompt = d.prompt();
    // The `claude` CLI reads its initial prompt from stdin when
    // invoked without arguments. Pipe the prompt in via a small
    // shell wrapper so the pane gets an interactive session
    // seeded with our message.
    //
    // We embed the prompt as a here-string to preserve newlines
    // without escaping quotes into the shell — 'EOF' quoting
    // stops variable expansion so backticks / $vars in a ticket
    // summary don't fire.
    let shell_cmd = format!("claude <<'MNML_EOF'\n{prompt}\nMNML_EOF");
    let payload = serde_json::json!({
        "cmd": "term",
        "args": ["sh", "-c", &shell_cmd],
    });
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&cmd_path)
        .with_context(|| format!("opening {}", cmd_path.display()))?;
    writeln!(f, "{payload}").with_context(|| format!("writing to {}", cmd_path.display()))?;
    Ok(())
}

/// ISO-8601 (UTC) timestamp of "now". Uses chrono because it's
/// already a dependency; keeps the format stable across platforms.
fn iso_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Given a `dispatch_workspace` root, return the `(queue_dir,
/// ipc_dir)` pair the dispatcher should write into: `<root>/.claude`
/// for the JSONL queue and `<root>/.mnml/ipc` for live IPC events.
/// Each is `Some(...)` only when the sub-directory exists on disk —
/// so a fresh clone / different user gets `None` and dispatch
/// falls back to whichever channel resolved.
///
/// Path is caller-supplied — usually from `Config::dispatch_workspace`
/// (top-level TOML key in `~/.config/mnml-tracker-jira.toml`).
/// Returns `(None, None)` when passed `None`.
pub fn workspace_dispatch_paths(root: Option<&Path>) -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(root) = root else {
        return (None, None);
    };
    let queue_dir = Some(root.join(".claude")).filter(|p| p.exists());
    let ipc_dir = Some(root.join(".mnml").join("ipc")).filter(|p| p.exists());
    (queue_dir, ipc_dir)
}

/// Pick the buttons to render on a ticket row given its type +
/// status. Returns bracketed labels in display order. Ticket-type
/// case matters (Jira returns "Story" / "Bug" / "Task" with those
/// exact casings) but we lowercase-match to be robust.
///
/// Rules:
///   - Story / Task in To Do / In Progress ⇒ [ Implement ] [ Triage ]
///   - Bug in To Do / In Progress ⇒ [ Fix ] [ Triage ]
///   - Any type in Testing ⇒ [ Test ]     (2026-08-21 user ask)
///   - Any type in In PR Review ⇒ [ Review ]  (2026-08-21 user ask)
///   - Bug Reopened ⇒ [ Triage ]
///   - Anything else ⇒ empty
pub fn buttons_for_ticket(issue: &Issue) -> Vec<TicketButton> {
    let ttype = issue
        .fields
        .issuetype
        .as_ref()
        .map(|t| t.name.to_ascii_lowercase())
        .unwrap_or_default();
    let status = issue
        .fields
        .status
        .as_ref()
        .map(|s| s.name.to_ascii_lowercase())
        .unwrap_or_default();
    let is_todo_or_in_progress = matches!(status.as_str(), "to do" | "open" | "in progress");
    let is_pr_review = matches!(
        status.as_str(),
        "in pr review" | "in code review" | "code review" | "pr review" | "in review"
    );
    let is_testing = status == "testing";
    if is_testing {
        return vec![TicketButton::Test];
    }
    if is_pr_review {
        return vec![TicketButton::Review];
    }
    match ttype.as_str() {
        "story" | "task" if is_todo_or_in_progress => {
            vec![TicketButton::Implement, TicketButton::Triage]
        }
        "bug" if is_todo_or_in_progress => vec![TicketButton::Fix, TicketButton::Triage],
        "bug" if status == "reopened" => vec![TicketButton::Triage],
        _ => Vec::new(),
    }
}

/// One action-button variant. Label + dispatch kind derive from
/// the variant via the impl below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketButton {
    Implement,
    Fix,
    Triage,
    /// #1110 f/u2 (2026-08-21) — surfaces on tickets whose status is
    /// `Testing`. Dispatch kind = `test` → falls through to the
    /// `/agents:tester` slash command via the catch-all in
    /// `Dispatch::prompt`.
    Test,
    /// #1110 f/u2 (2026-08-21) — surfaces on tickets whose status is
    /// `In PR Review`. Dispatch kind = `review` → same slash target
    /// as the per-PR-row Review button, but without a `pr_url`.
    Review,
}

impl TicketButton {
    /// Display label including brackets (e.g. `"[ Implement ]"`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Implement => "[ Implement ]",
            Self::Fix => "[ Fix ]",
            Self::Triage => "[ Triage ]",
            Self::Test => "[ Test ]",
            Self::Review => "[ Review ]",
        }
    }

    /// Dispatch kind — the string that lands in the JSONL payload
    /// and drives the fallback prompt's slash command.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Implement => "implement",
            Self::Fix => "fix",
            Self::Triage => "triage",
            Self::Test => "test",
            Self::Review => "review",
        }
    }

    /// 2026-07-26 — semantic color slot for the button label.
    /// UI layer maps to a Color enum. Kept as a slot string so
    /// this crate doesn't depend on ratatui.
    ///   Implement → green  (positive, "make it happen")
    ///   Fix       → red    (bug work, matches destructive tone)
    ///   Triage    → yellow (investigate, caution / TBD)
    ///   Test      → cyan   (validation / QA lane)
    ///   Review    → magenta (code review lane)
    pub fn color_slot(self) -> &'static str {
        match self {
            Self::Implement => "green",
            Self::Fix => "red",
            Self::Triage => "yellow",
            Self::Test => "cyan",
            Self::Review => "magenta",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::{Fields, Issue, NamedField};

    fn issue(key: &str, status: &str, ttype: &str, summary: &str) -> Issue {
        Issue {
            id: format!("id-{key}"),
            key: key.to_string(),
            fields: Fields {
                summary: summary.to_string(),
                status: Some(NamedField {
                    name: status.to_string(),
                }),
                assignee: None,
                reporter: None,
                priority: None,
                issuetype: Some(NamedField {
                    name: ttype.to_string(),
                }),
                updated: None,
                created: None,
                fix_versions: Vec::new(),
                components: Vec::new(),
                labels: Vec::new(),
                extras: std::collections::BTreeMap::new(),
            },
        }
    }

    #[test]
    fn story_in_to_do_gets_implement_and_triage() {
        let buttons = buttons_for_ticket(&issue("TE-1", "To Do", "Story", "s"));
        assert_eq!(buttons, vec![TicketButton::Implement, TicketButton::Triage]);
    }

    #[test]
    fn task_in_progress_gets_implement_and_triage() {
        let buttons = buttons_for_ticket(&issue("TE-1", "In Progress", "Task", "t"));
        assert_eq!(buttons, vec![TicketButton::Implement, TicketButton::Triage]);
    }

    #[test]
    fn bug_in_to_do_gets_fix_and_triage() {
        let buttons = buttons_for_ticket(&issue("TE-1", "To Do", "Bug", "b"));
        assert_eq!(buttons, vec![TicketButton::Fix, TicketButton::Triage]);
    }

    #[test]
    fn ticket_in_testing_gets_test() {
        let buttons = buttons_for_ticket(&issue("TE-1", "Testing", "Bug", "b"));
        assert_eq!(buttons, vec![TicketButton::Test]);
    }

    #[test]
    fn ticket_in_pr_review_gets_review() {
        let buttons = buttons_for_ticket(&issue("TE-1", "In PR Review", "Bug", "b"));
        assert_eq!(buttons, vec![TicketButton::Review]);
    }

    #[test]
    fn story_in_done_gets_no_buttons() {
        let buttons = buttons_for_ticket(&issue("TE-1", "Done", "Story", "s"));
        assert!(buttons.is_empty());
    }

    #[test]
    fn dispatch_for_ticket_populates_kind_and_type() {
        let iss = issue("TE-14337", "To Do", "Story", "add data-cy");
        let d = Dispatch::for_ticket(
            "implement",
            &iss,
            "https://example.atlassian.net/browse/PROJ-123".to_string(),
        );
        assert_eq!(d.kind, "implement");
        assert_eq!(d.issue_key, "TE-14337");
        assert_eq!(d.issue_type, "Story");
        assert!(d.pr_url.is_none());
    }

    #[test]
    fn dispatch_prompt_uses_developer_agent_for_implement() {
        let iss = issue("TE-14337", "To Do", "Story", "add data-cy");
        let d = Dispatch::for_ticket(
            "implement",
            &iss,
            "https://example.atlassian.net/browse/PROJ-123".to_string(),
        );
        let p = d.prompt();
        assert!(p.starts_with("/agents:developer TE-14337"));
        assert!(p.contains("TE-14337"));
        assert!(p.contains("add data-cy"));
    }

    #[test]
    fn dispatch_prompt_uses_reviewer_agent_for_review_with_pr_url() {
        let iss = issue("TE-14337", "In PR Review", "Story", "s");
        let d = Dispatch::for_pr(
            &iss,
            "https://example.atlassian.net/browse/PROJ-123".to_string(),
            "https://bitbucket.org/acme/foo/pull-requests/2023".to_string(),
        );
        assert!(
            d.prompt()
                .starts_with("/agents:reviewer https://bitbucket.org/acme/foo/pull-requests/2023")
        );
    }

    #[test]
    fn write_to_queue_appends_one_jsonl_line() {
        let tmp = tempfile::tempdir().unwrap();
        let iss = issue("TE-1", "To Do", "Story", "s");
        let d = Dispatch::for_ticket("implement", &iss, "https://x/browse/TE-1".to_string());
        write_to_queue(&d, tmp.path()).unwrap();
        write_to_queue(&d, tmp.path()).unwrap();
        let contents = std::fs::read_to_string(tmp.path().join("queue.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["kind"], "implement");
            assert_eq!(parsed["issue_key"], "TE-1");
        }
    }

    #[test]
    fn dispatch_all_channels_missing_reports_no_dispatch() {
        let iss = issue("TE-1", "To Do", "Story", "s");
        let d = Dispatch::for_ticket("implement", &iss, "https://x/browse/TE-1".to_string());
        let msg = dispatch(&d, None, None);
        assert!(msg.contains("no dispatch channels"));
    }
}
