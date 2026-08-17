//! Fix-Versions tree layout — pure functions.
//!
//! Takes a flat `Vec<Issue>` + `TreeState` + tab config and returns
//! a `Vec<VisibleRow>` that the renderer + cursor nav operate on.
//! All logic is pure so grouping / bumps / expansion can be
//! unit-tested without a running app.
//!
//! Terminology:
//!   - **Group** — a status bucket (e.g. "Testing"). Rendered as a
//!     header row with a chevron; ticket rows sit under it.
//!   - **Effective status** — the status a ticket is SORTED under
//!     after bump rules apply. May differ from `issue.fields.status`.
//!   - **Bump** — a conditional rule that promotes a ticket into a
//!     higher-priority group. See `BumpRules`.

use crate::config::{BumpRules, Config, Tab};
use crate::jira::{Issue, LinkedPr};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Per-tab tree state. `None` on non-tree tabs; `Some` on
/// `TabKind::FixVersionTree`. Populated at tab construction time,
/// mutated by expand/collapse actions.
#[derive(Debug, Clone, Default)]
pub struct TreeState {
    /// Status groups the user has collapsed. Absence = expanded
    /// (default — the "what can I act on" view wants everything
    /// visible upfront).
    pub collapsed_groups: HashSet<String>,
    /// Tickets whose linked-PR sub-tree is revealed. Key = issue.key.
    pub expanded_tickets: HashSet<String>,
    /// PRs whose pipeline sub-line is revealed. Key = (issue.key,
    /// linked_pr.id). Populated by the pipeline sub-flow — Right on
    /// a merged LinkedPr row inserts here + kicks off the fetch;
    /// Left removes.
    pub expanded_prs: HashSet<(String, String)>,
    /// Fetched linked PRs per ticket. Absent = not fetched yet or
    /// in flight. Present = ready (empty vec = no PRs linked).
    pub pr_cache: HashMap<String, Vec<LinkedPr>>,
    /// Fetched post-merge pipelines per (issue.key, pr.id). Absent
    /// = fetch in flight (renders as PrPipelineLoading); present +
    /// empty vec = "fetched, no pipeline ran on merge commit"
    /// (renders as PrPipelineEmpty); present + non-empty = one
    /// PrPipeline row per entry (newest first).
    pub pipeline_cache: HashMap<(String, String), Vec<crate::bitbucket::Pipeline>>,
    /// Terminal errors from a pipeline fetch, keyed the same way as
    /// `pipeline_cache`. Presence here overrides the "in flight"
    /// state so a failed fetch renders as PrPipelineError with the
    /// original message rather than spinning forever.
    pub pipeline_errors: HashMap<(String, String), String>,
    /// 2026-08-07 — per-ticket PR visibility cap. When a ticket has
    /// more than 3 linked PRs (common for long-running work), the
    /// first 3 render + a "▸ show 3 more" affordance stands in for
    /// the rest. Clicking that row bumps this count by 3.
    /// Absent = default of 3.
    pub pr_show_counts: HashMap<String, usize>,
}

/// One rendered row in a FixVersionTree tab. The renderer walks
/// this Vec top-to-bottom; the cursor `selected` is a plain index
/// into it. Adding a row variant is additive — cursor math doesn't
/// need to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleRow {
    /// Status group header — chevron + label + count.
    GroupHeader {
        status: String,
        count: usize,
        expanded: bool,
    },
    /// One ticket. `issue_idx` is a plain index into `TabState.issues`.
    /// `effective_status` = the group this ticket landed in (may
    /// differ from `issue.fields.status.name` when a bump applied).
    Ticket {
        issue_idx: usize,
        effective_status: String,
        was_bumped: bool,
    },
    /// One linked PR under an expanded ticket. `pr_idx` is an index
    /// into `TreeState.pr_cache[issue.key]`.
    LinkedPr { issue_idx: usize, pr_idx: usize },
    /// A "fetching linked PRs…" hint under an expanded ticket that
    /// has no `pr_cache` entry yet. Replaces itself with LinkedPr
    /// rows (or a "no linked PRs" hint) once the fetch resolves.
    PrLoading { issue_idx: usize },
    /// An "no linked PRs" hint under an expanded ticket whose fetch
    /// resolved to an empty vec.
    PrEmpty { issue_idx: usize },
    /// A "fetching pipeline…" hint under an expanded merged LinkedPr
    /// row. Replaced by PrPipeline / PrPipelineEmpty / PrPipelineError
    /// once the pipeline fetch resolves.
    PrPipelineLoading { issue_idx: usize, pr_idx: usize },
    /// A "no pipeline ran on merge commit" hint under an expanded
    /// LinkedPr whose pipeline fetch resolved to an empty vec.
    PrPipelineEmpty { issue_idx: usize, pr_idx: usize },
    /// A "pipeline lookup failed: {msg}" hint under an expanded
    /// LinkedPr whose pipeline fetch errored (URL not parseable,
    /// token missing, HTTP failure, PR not merged, etc). The full
    /// error message lives in `TreeState.pipeline_errors`; UI
    /// looks it up by (issue_key, pr_id).
    PrPipelineError { issue_idx: usize, pr_idx: usize },
    /// One post-merge pipeline row under an expanded merged LinkedPr.
    /// `pipeline_idx` is an index into
    /// `TreeState.pipeline_cache[(issue_key, pr_id)]`.
    PrPipeline {
        issue_idx: usize,
        pr_idx: usize,
        pipeline_idx: usize,
    },
    /// 2026-08-07 — "show more PRs" affordance when a ticket has
    /// more linked PRs than `pr_show_counts[issue.key]`. `hidden`
    /// is the count of PRs currently NOT rendered. Click bumps the
    /// visibility by 3 (see App::pr_show_more_focused).
    PrShowMore { issue_idx: usize, hidden: usize },
}

/// Compute the visible-row layout from a tab's issues + tree state.
/// Pure function — no mutation, no I/O.
///
/// Grouping rules (in order):
/// 1. Compute each ticket's `effective_status` via `apply_bumps`.
///    Bumps promote tickets from their raw status into a higher-
///    priority group when the rules match.
/// 2. Partition into status buckets.
/// 3. Emit groups in `tab.status_order` (default fallback if
///    unset). Statuses NOT in the order list drop to the end,
///    alphabetically.
/// 4. For each group: emit a header row, then (if expanded) its
///    ticket rows, then (for each expanded ticket) its PR sub-rows.
pub fn compute_visible_rows(
    issues: &[Issue],
    tree: &TreeState,
    tab: &Tab,
    cfg: &Config,
) -> Vec<VisibleRow> {
    // Step 1: effective status per issue (bumps applied).
    let effective: Vec<(String, bool)> = issues
        .iter()
        .map(|iss| {
            let raw = iss
                .fields
                .status
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            let bumped_to = tab
                .bumps
                .as_ref()
                .and_then(|b| apply_bumps(iss, &raw, b, tree, cfg));
            match bumped_to {
                Some(target) => (target, true),
                None => (raw, false),
            }
        })
        .collect();

    // Step 2: partition indices by effective status.
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, (status, _)) in effective.iter().enumerate() {
        groups.entry(status.clone()).or_default().push(idx);
    }

    // Step 3: emit in configured order, then alpha-sorted remainder.
    // Use the tab-level override when set; fall back to the built-in
    // default_status_order otherwise.
    let order = tab
        .status_order
        .clone()
        .unwrap_or_else(Config::default_status_order);
    let mut rows = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    for status in &order {
        if let Some(idxs) = groups.get(status) {
            emit_group(status, idxs, &effective, tree, &mut rows);
            emitted.insert(status.clone());
        }
    }
    // Remaining statuses (not in the order list) — alpha at the end.
    for (status, idxs) in &groups {
        if !emitted.contains(status) {
            emit_group(status, idxs, &effective, tree, &mut rows);
        }
    }
    rows
}

/// Emit one status group into `out`: header row, then (if expanded)
/// ticket rows, then (for each expanded ticket) its PR sub-rows.
fn emit_group(
    status: &str,
    idxs: &[usize],
    effective: &[(String, bool)],
    tree: &TreeState,
    out: &mut Vec<VisibleRow>,
) {
    let expanded = !tree.collapsed_groups.contains(status);
    out.push(VisibleRow::GroupHeader {
        status: status.to_string(),
        count: idxs.len(),
        expanded,
    });
    if !expanded {
        return;
    }
    for &issue_idx in idxs {
        let (eff, bumped) = &effective[issue_idx];
        out.push(VisibleRow::Ticket {
            issue_idx,
            effective_status: eff.clone(),
            was_bumped: *bumped,
        });
        // For expanded tickets, emit their PR sub-rows (or a
        // loading/empty hint). We look up by INDEX rather than key
        // here because callers may pass a scratch Issue list; the
        // key/tree lookup happens in a sibling helper the caller
        // uses to seed `pr_cache`.
        //
        // NOTE: this fn can't look up by `issue.key` without the
        // full Issue — we defer that to caller-side plumbing at
        // render time. For now, tickets expanded flag is checked
        // by index-based key lookup in the pure helper below.
    }
}

/// Extract the linked-PR sub-rows for a ticket that's been expanded.
/// Called by the renderer after `compute_visible_rows` to
/// interleave sub-rows without threading Issue references through
/// the pure logic layer. Returns `Vec<VisibleRow>` — one of
/// PrLoading (cache miss), PrEmpty (empty vec cached), or a series
/// of LinkedPr rows.
///
/// Split out so `compute_visible_rows` stays index-based + easily
/// testable, and the caller does the two-step (compute rows, then
/// splice sub-rows in-place per expanded ticket).
pub fn expand_ticket_sub_rows(
    issue_idx: usize,
    issue: &Issue,
    tree: &TreeState,
) -> Vec<VisibleRow> {
    if !tree.expanded_tickets.contains(&issue.key) {
        return Vec::new();
    }
    match tree.pr_cache.get(&issue.key) {
        None => vec![VisibleRow::PrLoading { issue_idx }],
        Some(prs) if prs.is_empty() => vec![VisibleRow::PrEmpty { issue_idx }],
        Some(prs) => {
            // 2026-08-07 — cap at pr_show_counts[key] (default 3). PRs
            // arrive from Jira's dev-status API in whatever order the
            // repo returned them; we render newest-first by grabbing
            // the tail so long-running tickets don't drown the tree.
            let cap = tree.pr_show_counts.get(&issue.key).copied().unwrap_or(3);
            if prs.len() <= cap {
                (0..prs.len())
                    .map(|pr_idx| VisibleRow::LinkedPr { issue_idx, pr_idx })
                    .collect()
            } else {
                // Show the LAST `cap` PRs (newest = most recently
                // fetched, at the end of the vec). Then a "show more"
                // affordance for the older ones on top.
                let start = prs.len() - cap;
                let mut out: Vec<VisibleRow> = (start..prs.len())
                    .map(|pr_idx| VisibleRow::LinkedPr { issue_idx, pr_idx })
                    .collect();
                out.push(VisibleRow::PrShowMore {
                    issue_idx,
                    hidden: prs.len() - cap,
                });
                out
            }
        }
    }
}

/// Extract the pipeline sub-rows for a merged LinkedPr that's been
/// expanded (i.e. `(issue_key, pr_id)` is in `tree.expanded_prs`).
/// Called by the renderer during the sub-row splice pass, after
/// LinkedPr rows have been laid out. Returns empty when the PR
/// isn't expanded (the common case).
///
/// Priority when both `pipeline_errors` and `pipeline_cache` are
/// populated: the error wins (a terminal failure is what the user
/// needs to see; a stale cache would be misleading). Absence of
/// both = "fetch in flight" = PrPipelineLoading.
pub fn expand_pr_sub_rows(
    issue_idx: usize,
    pr_idx: usize,
    issue_key: &str,
    pr_id: &str,
    tree: &TreeState,
) -> Vec<VisibleRow> {
    let key = (issue_key.to_string(), pr_id.to_string());
    if !tree.expanded_prs.contains(&key) {
        return Vec::new();
    }
    if tree.pipeline_errors.contains_key(&key) {
        return vec![VisibleRow::PrPipelineError { issue_idx, pr_idx }];
    }
    match tree.pipeline_cache.get(&key) {
        None => vec![VisibleRow::PrPipelineLoading { issue_idx, pr_idx }],
        Some(pipelines) if pipelines.is_empty() => {
            vec![VisibleRow::PrPipelineEmpty { issue_idx, pr_idx }]
        }
        Some(pipelines) => (0..pipelines.len())
            .map(|pipeline_idx| VisibleRow::PrPipeline {
                issue_idx,
                pr_idx,
                pipeline_idx,
            })
            .collect(),
    }
}

/// Splice sub-rows in-place after each Ticket (and after each
/// LinkedPr for the pipeline drill-down). Called by the renderer as
/// a second pass. Two passes intentionally not merged into one:
/// the ticket-level splice runs first so LinkedPr rows exist to be
/// walked; only then does the pipeline splice see them and expand.
///
/// Kept separate from `compute_visible_rows` so the pure logic
/// stays testable without needing the full `&[Issue]`.
pub fn splice_ticket_sub_rows(rows: &mut Vec<VisibleRow>, issues: &[Issue], tree: &TreeState) {
    // Pass 1: splice PR rows under each expanded Ticket.
    let mut i = 0;
    while i < rows.len() {
        if let VisibleRow::Ticket { issue_idx, .. } = &rows[i] {
            let issue_idx = *issue_idx;
            let sub = expand_ticket_sub_rows(issue_idx, &issues[issue_idx], tree);
            if !sub.is_empty() {
                let n = sub.len();
                rows.splice(i + 1..i + 1, sub);
                i += n; // skip the just-inserted sub-rows
            }
        }
        i += 1;
    }
    // Pass 2: splice pipeline rows under each expanded LinkedPr.
    // Walks the (now-larger) list; because pipeline sub-rows are
    // never LinkedPr / Ticket variants, no risk of re-splicing what
    // we just inserted.
    let mut i = 0;
    while i < rows.len() {
        if let VisibleRow::LinkedPr { issue_idx, pr_idx } = &rows[i] {
            let issue_idx = *issue_idx;
            let pr_idx = *pr_idx;
            let issue_key = issues[issue_idx].key.as_str();
            let pr_id = tree
                .pr_cache
                .get(issue_key)
                .and_then(|prs| prs.get(pr_idx))
                .map(|pr| pr.id.as_str());
            if let Some(pr_id) = pr_id {
                let sub = expand_pr_sub_rows(issue_idx, pr_idx, issue_key, pr_id, tree);
                if !sub.is_empty() {
                    let n = sub.len();
                    rows.splice(i + 1..i + 1, sub);
                    i += n;
                }
            }
        }
        i += 1;
    }
}

/// Return the effective-status target for `issue` when a bump rule
/// applies, or `None` when no bump matches (caller uses the raw
/// status in that case).
///
/// Rule priority (first match wins):
/// 1. `release_cut[raw_status]` — active only when `cfg.release_cut`
///    is true. Special value `"top"` returns a sentinel `__TOP__`
///    which the sort layer honors as "before every configured
///    status". Any other value = a target status name.
/// 2. `pr_approved` — matches when the ticket is in a "PR Review"
///    style status AND at least one cached linked PR has an
///    approving reviewer.
/// 3. `no_open_prs` — matches when the ticket is in a "PR Review"
///    style status AND no cached linked PR is still open/draft.
///    Signal: dev merged the PR but forgot to transition the
///    ticket. Requires ≥1 non-open PR to fire (empty pr_cache is
///    "unknown, don't bump" so freshly-fetched tickets aren't
///    misclassified).
///
/// Rules 2/3 look at `tree.pr_cache` — an unexpanded ticket has
/// no cached PRs and thus can't be bumped by them. That's a
/// feature: bumps only take effect once the user opens the ticket,
/// avoiding a synchronous fan-out on every render.
pub fn apply_bumps(
    issue: &Issue,
    raw_status: &str,
    bumps: &BumpRules,
    tree: &TreeState,
    cfg: &Config,
) -> Option<String> {
    // Rule 1: release_cut context.
    if cfg.release_cut
        && let Some(target) = bumps.release_cut.get(raw_status)
    {
        return Some(match target.as_str() {
            "top" => TOP_SENTINEL.to_string(),
            other => other.to_string(),
        });
    }
    // Rules 2 + 3 apply only to PR-review-style statuses.
    if !is_pr_review_status(raw_status) {
        return None;
    }
    let prs = tree.pr_cache.get(&issue.key)?;
    // Rule 2: pr_approved.
    if let Some(target) = &bumps.pr_approved
        && prs.iter().any(|p| p.is_approved())
    {
        return Some(target.clone());
    }
    // Rule 3: no_open_prs. Requires ≥1 PR total AND zero open.
    if let Some(target) = &bumps.no_open_prs
        && !prs.is_empty()
        && !prs.iter().any(|p| p.is_open())
    {
        return Some(target.clone());
    }
    None
}

/// Sentinel status name for the release_cut "top" bump. Callers
/// treat this as "before every other group" when ordering.
pub const TOP_SENTINEL: &str = "__TOP__";

/// True when `status` is a "PR Review"-family status that the
/// bump rules should consider. Match is case-insensitive against
/// a small set of common labels; project-specific labels can be
/// added when they surface.
fn is_pr_review_status(status: &str) -> bool {
    let n = status.to_ascii_lowercase();
    n == "in pr review"
        || n == "in code review"
        || n == "code review"
        || n == "pr review"
        || n == "in review"
        || n == "awaiting review"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BumpRules, Config, Tab, TabKind};
    use crate::jira::{
        Fields, Issue, LinkedPr, LinkedPrAuthor, LinkedPrBranch, LinkedPrReviewer, NamedField,
    };

    fn issue(key: &str, status: &str) -> Issue {
        Issue {
            id: format!("id-{key}"),
            key: key.to_string(),
            fields: Fields {
                summary: format!("{key} summary"),
                status: Some(NamedField {
                    name: status.to_string(),
                }),
                assignee: None,
                reporter: None,
                priority: None,
                issuetype: None,
                updated: None,
                created: None,
                fix_versions: Vec::new(),
                components: Vec::new(),
                labels: Vec::new(),
                extras: std::collections::BTreeMap::new(),
            },
        }
    }

    fn linked_pr(status: &str, approvers: usize) -> LinkedPr {
        LinkedPr {
            id: "#1".to_string(),
            name: "test PR".to_string(),
            status: status.to_string(),
            url: String::new(),
            source: LinkedPrBranch::default(),
            destination: LinkedPrBranch::default(),
            reviewers: (0..approvers)
                .map(|i| LinkedPrReviewer {
                    name: format!("r{i}"),
                    approved: true,
                })
                .collect(),
            author: LinkedPrAuthor::default(),
            last_update: String::new(),
            repository_name: String::new(),
        }
    }

    fn fix_version_tab() -> Tab {
        Tab {
            name: "Current".to_string(),
            kind: Some(TabKind::FixVersionTree),
            mode: None,
            jql: None,
            project: Some("TE".to_string()),
            component: None,
            columns: None,
            status_order: None,
            bumps: None,
            version_name_contains: None,
            team: None,
            board_id: None,
        }
    }

    fn default_cfg() -> Config {
        Config {
            jira_url: "https://x.atlassian.net".to_string(),
            email: "a@b.c".to_string(),
            refresh_interval_secs: 60,
            tabs: Vec::new(),
            release_cut: false,
            team_field_id: None,
            team_field_name: None,
            dispatch_workspace: None,
            detail_modal: crate::config::DetailModalConfig::default(),
        }
    }

    #[test]
    fn compute_visible_rows_groups_by_status_using_default_order() {
        let issues = vec![
            issue("TE-1", "Done"),
            issue("TE-2", "Testing"),
            issue("TE-3", "To Do"),
        ];
        let tree = TreeState::default();
        let rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        // Default order = [Testing, In PR Review, Code Review,
        // In Progress, To Do, Open, Done]. Testing comes first.
        assert!(matches!(
            &rows[0],
            VisibleRow::GroupHeader { status, .. } if status == "Testing"
        ));
        // Last header should be Done.
        let last_header = rows
            .iter()
            .rev()
            .find_map(|r| match r {
                VisibleRow::GroupHeader { status, .. } => Some(status),
                _ => None,
            })
            .unwrap();
        assert_eq!(last_header, "Done");
    }

    #[test]
    fn compute_visible_rows_hides_ticket_rows_for_collapsed_groups() {
        let issues = vec![issue("TE-1", "Testing"), issue("TE-2", "Done")];
        let mut tree = TreeState::default();
        tree.collapsed_groups.insert("Done".to_string());
        let rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        // Two headers, one ticket (Testing only — Done is collapsed).
        let headers = rows
            .iter()
            .filter(|r| matches!(r, VisibleRow::GroupHeader { .. }))
            .count();
        let tickets = rows
            .iter()
            .filter(|r| matches!(r, VisibleRow::Ticket { .. }))
            .count();
        assert_eq!(headers, 2);
        assert_eq!(tickets, 1);
    }

    #[test]
    fn custom_status_order_wins_over_default() {
        let issues = vec![issue("TE-1", "To Do"), issue("TE-2", "Testing")];
        let mut tab = fix_version_tab();
        tab.status_order = Some(vec!["To Do".to_string(), "Testing".to_string()]);
        let rows = compute_visible_rows(&issues, &TreeState::default(), &tab, &default_cfg());
        assert!(matches!(
            &rows[0],
            VisibleRow::GroupHeader { status, .. } if status == "To Do"
        ));
    }

    #[test]
    fn unknown_statuses_land_at_the_bottom_alpha_sorted() {
        let issues = vec![
            issue("TE-1", "Testing"),
            issue("TE-2", "Zombie"),
            issue("TE-3", "Aardvark"),
        ];
        let rows = compute_visible_rows(
            &issues,
            &TreeState::default(),
            &fix_version_tab(),
            &default_cfg(),
        );
        let headers: Vec<&String> = rows
            .iter()
            .filter_map(|r| match r {
                VisibleRow::GroupHeader { status, .. } => Some(status),
                _ => None,
            })
            .collect();
        // Testing is in the default order; Aardvark + Zombie aren't,
        // so they land at the end sorted alpha (Aardvark then Zombie).
        assert_eq!(headers, vec!["Testing", "Aardvark", "Zombie"]);
    }

    #[test]
    fn pr_approved_bump_promotes_ticket_from_pr_review_to_testing() {
        let issues = vec![issue("TE-1", "In PR Review")];
        let mut tree = TreeState::default();
        tree.pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("OPEN", 2)]);
        let mut tab = fix_version_tab();
        tab.bumps = Some(BumpRules {
            pr_approved: Some("Testing".to_string()),
            no_open_prs: None,
            release_cut: Default::default(),
        });
        let rows = compute_visible_rows(&issues, &tree, &tab, &default_cfg());
        // Ticket sits under the Testing header, not In PR Review.
        // Also carries `was_bumped: true`.
        let ticket = rows
            .iter()
            .find_map(|r| match r {
                VisibleRow::Ticket {
                    effective_status,
                    was_bumped,
                    ..
                } => Some((effective_status.as_str(), *was_bumped)),
                _ => None,
            })
            .unwrap();
        assert_eq!(ticket, ("Testing", true));
    }

    #[test]
    fn no_open_prs_bump_fires_only_when_prs_exist_and_all_are_closed() {
        // Cache empty ⇒ don't bump (unknown, defer).
        let issues = vec![issue("TE-1", "In PR Review")];
        let tree = TreeState::default();
        let mut tab = fix_version_tab();
        tab.bumps = Some(BumpRules {
            pr_approved: None,
            no_open_prs: Some("Testing".to_string()),
            release_cut: Default::default(),
        });
        let rows = compute_visible_rows(&issues, &tree, &tab, &default_cfg());
        let effective = match &rows[1] {
            VisibleRow::Ticket {
                effective_status, ..
            } => effective_status.as_str(),
            _ => panic!("expected ticket row"),
        };
        assert_eq!(effective, "In PR Review", "no cache = no bump");

        // Cache has one MERGED PR ⇒ bump (dev forgot to transition).
        let mut tree2 = TreeState::default();
        tree2
            .pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("MERGED", 0)]);
        let rows2 = compute_visible_rows(&issues, &tree2, &tab, &default_cfg());
        let effective2 = match &rows2[1] {
            VisibleRow::Ticket {
                effective_status, ..
            } => effective_status.as_str(),
            _ => panic!("expected ticket row"),
        };
        assert_eq!(effective2, "Testing");
    }

    #[test]
    fn release_cut_bump_only_fires_when_config_flag_set() {
        let issues = vec![issue("TE-1", "Done")];
        let mut tab = fix_version_tab();
        let mut release_cut_map = std::collections::HashMap::new();
        release_cut_map.insert("Done".to_string(), "top".to_string());
        tab.bumps = Some(BumpRules {
            pr_approved: None,
            no_open_prs: None,
            release_cut: release_cut_map,
        });
        // Flag off — no bump.
        let cfg_off = default_cfg();
        let rows_off = compute_visible_rows(&issues, &TreeState::default(), &tab, &cfg_off);
        assert!(matches!(
            &rows_off[0],
            VisibleRow::GroupHeader { status, .. } if status == "Done"
        ));

        // Flag on — bump to TOP_SENTINEL.
        let mut cfg_on = default_cfg();
        cfg_on.release_cut = true;
        let rows_on = compute_visible_rows(&issues, &TreeState::default(), &tab, &cfg_on);
        let first_header = rows_on
            .iter()
            .find_map(|r| match r {
                VisibleRow::GroupHeader { status, .. } => Some(status.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(first_header, TOP_SENTINEL);
    }

    #[test]
    fn splice_ticket_sub_rows_emits_loading_hint_when_cache_missing() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        // pr_cache is absent → PrLoading.
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        let has_loading = rows
            .iter()
            .any(|r| matches!(r, VisibleRow::PrLoading { .. }));
        assert!(has_loading);
    }

    #[test]
    fn splice_ticket_sub_rows_emits_empty_hint_when_cache_empty_vec() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache.insert("TE-1".to_string(), Vec::new());
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        let has_empty = rows.iter().any(|r| matches!(r, VisibleRow::PrEmpty { .. }));
        assert!(has_empty);
    }

    #[test]
    fn splice_ticket_sub_rows_emits_one_row_per_linked_pr() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache.insert(
            "TE-1".to_string(),
            vec![linked_pr("OPEN", 0), linked_pr("MERGED", 1)],
        );
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        let pr_rows = rows
            .iter()
            .filter(|r| matches!(r, VisibleRow::LinkedPr { .. }))
            .count();
        assert_eq!(pr_rows, 2);
    }

    #[test]
    fn splice_leaves_collapsed_tickets_untouched() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        // NOT expanded → no sub-rows even if cache is populated.
        tree.pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("OPEN", 0)]);
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        assert!(
            rows.iter()
                .all(|r| !matches!(r, VisibleRow::LinkedPr { .. }))
        );
    }

    // ── Pipeline sub-row splice ────────────────────────────────────

    #[test]
    fn splice_pipelines_emits_loading_when_expanded_but_cache_absent() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("MERGED", 0)]);
        // Expand the PR — cache miss ⇒ PrPipelineLoading.
        tree.expanded_prs
            .insert(("TE-1".to_string(), "#1".to_string()));
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        assert!(
            rows.iter()
                .any(|r| matches!(r, VisibleRow::PrPipelineLoading { .. })),
            "expected PrPipelineLoading in {rows:?}"
        );
    }

    #[test]
    fn splice_pipelines_emits_empty_when_cache_is_empty_vec() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("MERGED", 0)]);
        tree.expanded_prs
            .insert(("TE-1".to_string(), "#1".to_string()));
        tree.pipeline_cache
            .insert(("TE-1".to_string(), "#1".to_string()), Vec::new());
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        assert!(
            rows.iter()
                .any(|r| matches!(r, VisibleRow::PrPipelineEmpty { .. })),
            "expected PrPipelineEmpty in {rows:?}"
        );
    }

    #[test]
    fn splice_pipelines_emits_error_when_errors_map_populated() {
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("MERGED", 0)]);
        tree.expanded_prs
            .insert(("TE-1".to_string(), "#1".to_string()));
        tree.pipeline_errors.insert(
            ("TE-1".to_string(), "#1".to_string()),
            "not a bitbucket PR URL".to_string(),
        );
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        assert!(
            rows.iter()
                .any(|r| matches!(r, VisibleRow::PrPipelineError { .. })),
            "expected PrPipelineError in {rows:?}"
        );
    }

    #[test]
    fn splice_pipelines_leaves_collapsed_pr_untouched() {
        // Ticket expanded + linked-PR row rendered, but PR itself
        // NOT expanded → no pipeline sub-rows.
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache
            .insert("TE-1".to_string(), vec![linked_pr("MERGED", 0)]);
        tree.pipeline_cache
            .insert(("TE-1".to_string(), "#1".to_string()), Vec::new());
        // NOTE: expanded_prs is empty.
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        assert!(rows.iter().all(|r| !matches!(
            r,
            VisibleRow::PrPipelineLoading { .. }
                | VisibleRow::PrPipelineEmpty { .. }
                | VisibleRow::PrPipelineError { .. }
                | VisibleRow::PrPipeline { .. }
        )));
    }

    #[test]
    fn splice_pipelines_row_order_is_pr_then_pipelines_then_next_pr() {
        // Verify the splice puts pipeline rows *between* consecutive
        // LinkedPr rows in the right place — under the parent PR,
        // above the next PR.
        let issues = vec![issue("TE-1", "Testing")];
        let mut tree = TreeState::default();
        tree.expanded_tickets.insert("TE-1".to_string());
        tree.pr_cache.insert(
            "TE-1".to_string(),
            vec![linked_pr("MERGED", 0), linked_pr("MERGED", 0)],
        );
        // Both LinkedPrs share id `#1` in the test factory — so
        // expand just one via key equality (both share the key, so
        // both technically expand; that's fine for row-ordering).
        tree.expanded_prs
            .insert(("TE-1".to_string(), "#1".to_string()));
        // One pipeline in the cache.
        tree.pipeline_cache.insert(
            ("TE-1".to_string(), "#1".to_string()),
            vec![crate::bitbucket::Pipeline {
                uuid: "u".into(),
                build_number: 1234,
                state: None,
                created_on: None,
                duration_in_seconds: None,
                target: None,
            }],
        );
        let mut rows = compute_visible_rows(&issues, &tree, &fix_version_tab(), &default_cfg());
        splice_ticket_sub_rows(&mut rows, &issues, &tree);
        let variants: Vec<&'static str> = rows
            .iter()
            .map(|r| match r {
                VisibleRow::GroupHeader { .. } => "H",
                VisibleRow::Ticket { .. } => "T",
                VisibleRow::LinkedPr { .. } => "P",
                VisibleRow::PrLoading { .. } => "L",
                VisibleRow::PrEmpty { .. } => "E",
                VisibleRow::PrPipelineLoading { .. } => "l",
                VisibleRow::PrPipelineEmpty { .. } => "e",
                VisibleRow::PrPipelineError { .. } => "x",
                VisibleRow::PrPipeline { .. } => "p",
                VisibleRow::PrShowMore { .. } => "M",
            })
            .collect();
        // Header, Ticket, PR, pipeline, PR, pipeline. Adjacency
        // rules — every "p" must sit immediately after a "P".
        for (i, v) in variants.iter().enumerate() {
            if *v == "p" {
                assert!(i > 0);
                let prev = variants[i - 1];
                assert!(prev == "P" || prev == "p", "prev of `p` was {prev}");
            }
        }
    }
}
