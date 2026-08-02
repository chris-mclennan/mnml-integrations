//! Document-result renderer (`QueryResult::Documents`) — first real
//! user is the DocDB / DynamoDB drivers.
//!
//! Renders `Vec<serde_json::Value>` as an expand/collapse tree using
//! the same visual language as `schema_tree` (▶/▼ chevrons for
//! containers, `▸` cursor marker on the focused row, cyan-bold title
//! when the pane has focus).
//!
//! Interaction model (v0.1):
//!   * Up / Down     — walk visible rows (`app.result_row`).
//!   * PgUp / PgDn   — 10 rows at a time.
//!   * Enter         — expand a container row (a document, an object,
//!     or an array). No-op on leaf rows.
//!   * Left / Esc    — collapse the container the cursor sits inside.
//!
//! The visible-line list is rebuilt on every render from the current
//! `app.result` + `app.tree.expanded` set. Expansion state is keyed
//! by a path string (`"3.address.city"`, `"3.tags[1]"`) so it stays
//! stable across re-renders even if the result is filtered.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, Focus};
use crate::driver::QueryResult;
use crate::ui::themed;

/// Build the flat visible-line list from a document array + the
/// expanded-path set.
pub fn build_lines(
    docs: &[serde_json::Value],
    expanded: &std::collections::BTreeSet<String>,
) -> Vec<TreeLine> {
    let mut out = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let path = i.to_string();
        let (icon, summary) = doc_summary(d);
        let is_open = expanded.contains(&path);
        out.push(TreeLine {
            depth: 0,
            path: path.clone(),
            kind: LineKind::Container {
                open: is_open,
                icon,
            },
            label: format!("[{i}]"),
            summary,
        });
        if is_open {
            walk_value(d, &path, 1, expanded, &mut out);
        }
    }
    out
}

fn walk_value(
    v: &serde_json::Value,
    prefix: &str,
    depth: usize,
    expanded: &std::collections::BTreeSet<String>,
    out: &mut Vec<TreeLine>,
) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, sub) in m {
                let path = format!("{prefix}.{k}");
                push_row(k, sub, &path, depth, expanded, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, sub) in arr.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                push_row(&format!("[{i}]"), sub, &path, depth, expanded, out);
            }
        }
        // Primitive at root of a walk — render as a single labelled
        // leaf. Reached only via a runCommand result that isn't a
        // container.
        other => out.push(TreeLine {
            depth,
            path: prefix.to_string(),
            kind: LineKind::Leaf,
            label: String::new(),
            summary: leaf_display(other),
        }),
    }
}

fn push_row(
    label: &str,
    v: &serde_json::Value,
    path: &str,
    depth: usize,
    expanded: &std::collections::BTreeSet<String>,
    out: &mut Vec<TreeLine>,
) {
    match v {
        serde_json::Value::Object(m) => {
            let is_open = expanded.contains(path);
            out.push(TreeLine {
                depth,
                path: path.to_string(),
                kind: LineKind::Container {
                    open: is_open,
                    icon: '{',
                },
                label: label.to_string(),
                summary: format!("{{{} fields}}", m.len()),
            });
            if is_open {
                walk_value(v, path, depth + 1, expanded, out);
            }
        }
        serde_json::Value::Array(arr) => {
            let is_open = expanded.contains(path);
            out.push(TreeLine {
                depth,
                path: path.to_string(),
                kind: LineKind::Container {
                    open: is_open,
                    icon: '[',
                },
                label: label.to_string(),
                summary: format!("[{} items]", arr.len()),
            });
            if is_open {
                walk_value(v, path, depth + 1, expanded, out);
            }
        }
        other => out.push(TreeLine {
            depth,
            path: path.to_string(),
            kind: LineKind::Leaf,
            label: label.to_string(),
            summary: leaf_display(other),
        }),
    }
}

fn doc_summary(v: &serde_json::Value) -> (char, String) {
    match v {
        serde_json::Value::Object(m) => {
            // Prefer `_id` / `id` in the summary, falling back to the
            // first field.
            let (k, first) = m
                .iter()
                .find(|(k, _)| k.as_str() == "_id" || k.as_str() == "id")
                .or_else(|| m.iter().next())
                .map(|(k, v)| (k.clone(), v))
                .unwrap_or_else(|| ("_".into(), &serde_json::Value::Null));
            let val = one_line(&leaf_display(first), 40);
            ('{', format!("{{{} fields}} · {k}={val}", m.len()))
        }
        serde_json::Value::Array(arr) => ('[', format!("[{} items]", arr.len())),
        other => ('•', leaf_display(other)),
    }
}

fn leaf_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("\"{s}\""),
        // Containers are handled by push_row / walk_value — this arm
        // is only reachable for a top-level scalar leaf.
        other => other.to_string(),
    }
}

fn one_line(s: &str, n: usize) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() <= n {
        return flat;
    }
    let mut out: String = flat.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone)]
pub struct TreeLine {
    pub depth: usize,
    pub path: String,
    pub kind: LineKind,
    /// The key / index shown to the left of the summary.
    pub label: String,
    /// Preview text — `{N fields}` for objects, `[N items]` for
    /// arrays, `"..."` for strings, etc.
    pub summary: String,
}

#[derive(Debug, Clone, Copy)]
pub enum LineKind {
    Container { open: bool, icon: char },
    Leaf,
}

impl TreeLine {
    pub fn is_container(&self) -> bool {
        matches!(self.kind, LineKind::Container { .. })
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Results;
    let Some(QueryResult::Documents { docs, elapsed_ms }) = &app.result else {
        return;
    };
    let expanded = &app.doc_expanded;
    let lines = build_lines(docs, expanded);

    let title = format!(" documents ({} · {}ms) ", docs.len(), elapsed_ms);
    let title_style = if focused {
        Style::default()
            .fg(themed(Color::Cyan))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(themed(Color::DarkGray))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, title_style));

    if docs.is_empty() {
        let p = Paragraph::new("(no documents)")
            .style(Style::default().fg(themed(Color::DarkGray)))
            .block(block);
        f.render_widget(p, area);
        return;
    }

    // Scroll so the selected row stays in view. Height = area - 2
    // (borders).
    let visible_rows = area.height.saturating_sub(2) as usize;
    let cursor = app.result_row.min(lines.len().saturating_sub(1));
    let scroll = compute_scroll(cursor, lines.len(), visible_rows);

    let mut rendered: Vec<Line> = Vec::with_capacity(visible_rows);
    for (i, line) in lines.iter().enumerate().skip(scroll).take(visible_rows) {
        let selected = i == cursor && focused;
        let marker = if selected { "▸ " } else { "  " };
        let indent: String = "  ".repeat(line.depth);
        let chev = match line.kind {
            LineKind::Container { open: true, .. } => "▼",
            LineKind::Container { open: false, .. } => "▶",
            LineKind::Leaf => " ",
        };
        let icon = match line.kind {
            LineKind::Container { icon, .. } => icon,
            LineKind::Leaf => ' ',
        };
        let label_style = if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(themed(Color::Yellow))
        };
        let summary_style = Style::default().fg(themed(Color::Gray));
        let icon_style = Style::default().fg(themed(Color::DarkGray));
        rendered.push(Line::from(vec![
            Span::raw(marker),
            Span::raw(indent),
            Span::styled(format!("{chev} "), icon_style),
            Span::styled(format!("{icon} "), icon_style),
            Span::styled(format!("{}  ", line.label), label_style),
            Span::styled(line.summary.clone(), summary_style),
        ]));
    }
    let p = Paragraph::new(rendered).block(block);
    f.render_widget(p, area);
}

fn compute_scroll(cursor: usize, total: usize, viewport: usize) -> usize {
    if viewport == 0 || total <= viewport {
        return 0;
    }
    if cursor < viewport / 2 {
        return 0;
    }
    let max_scroll = total - viewport;
    (cursor - viewport / 2).min(max_scroll)
}

/// Expand or collapse the container at `cursor`. No-op on a leaf row.
/// Returns the new cursor (unchanged in v0.1 — expansion pushes rows
/// out below the cursor, which is fine).
pub fn toggle_at(app: &mut App, cursor: usize) -> usize {
    let Some(QueryResult::Documents { docs, .. }) = &app.result else {
        return cursor;
    };
    let lines = build_lines(docs, &app.doc_expanded);
    let Some(line) = lines.get(cursor) else {
        return cursor;
    };
    if line.is_container() {
        if app.doc_expanded.contains(&line.path) {
            app.doc_expanded.remove(&line.path);
        } else {
            app.doc_expanded.insert(line.path.clone());
        }
    }
    cursor
}

/// Collapse the container the cursor sits inside — either the row
/// itself (if it's a container) or its parent (if it's a leaf inside
/// one). Then move the cursor to the collapsed row so subsequent
/// arrow-navigation feels right.
pub fn collapse_at(app: &mut App, cursor: usize) -> usize {
    let Some(QueryResult::Documents { docs, .. }) = &app.result else {
        return cursor;
    };
    let lines = build_lines(docs, &app.doc_expanded);
    let Some(line) = lines.get(cursor) else {
        return cursor;
    };
    match line.kind {
        LineKind::Container { open: true, .. } => {
            app.doc_expanded.remove(&line.path);
            cursor
        }
        _ => {
            // Find nearest open container ancestor. The parent's path
            // is `line.path` with the last `.<key>` or `[i]` chunk
            // trimmed.
            let parent = parent_path(&line.path);
            if let Some(p) = parent {
                app.doc_expanded.remove(&p);
                // Move cursor to the ancestor row.
                if let Some(pos) = lines.iter().position(|l| l.path == p) {
                    return pos;
                }
            }
            cursor
        }
    }
}

fn parent_path(path: &str) -> Option<String> {
    // Handle `[i]` first — it's the array-index suffix.
    if let Some(open) = path.rfind('[')
        && path.ends_with(']')
    {
        let head = &path[..open];
        if head.is_empty() {
            return None;
        }
        return Some(head.to_string());
    }
    if let Some(dot) = path.rfind('.') {
        return Some(path[..dot].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> std::collections::BTreeSet<String> {
        Default::default()
    }

    #[test]
    fn empty_array_produces_no_lines() {
        let lines = build_lines(&[], &set());
        assert!(lines.is_empty());
    }

    #[test]
    fn single_collapsed_doc_is_one_row() {
        let docs = vec![serde_json::json!({ "_id": 1, "name": "Alice" })];
        let lines = build_lines(&docs, &set());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].label, "[0]");
        assert!(lines[0].summary.contains("_id="));
    }

    #[test]
    fn expanded_doc_shows_field_rows() {
        let docs = vec![serde_json::json!({ "a": 1, "b": "s" })];
        let mut e = set();
        e.insert("0".into());
        let lines = build_lines(&docs, &e);
        // Root + 2 fields = 3 rows.
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].label, "a");
        assert_eq!(lines[1].summary, "1");
        assert_eq!(lines[2].label, "b");
        assert_eq!(lines[2].summary, "\"s\"");
    }

    #[test]
    fn deeply_nested_doc_walks_when_all_expanded() {
        let docs = vec![serde_json::json!({
            "outer": { "inner": { "leaf": 42 } }
        })];
        let mut e = set();
        e.insert("0".into());
        e.insert("0.outer".into());
        e.insert("0.outer.inner".into());
        let lines = build_lines(&docs, &e);
        // root + outer + inner + leaf = 4 rows.
        assert_eq!(lines.len(), 4);
        assert_eq!(lines.last().unwrap().summary, "42");
        assert_eq!(lines.last().unwrap().depth, 3);
    }

    #[test]
    fn huge_doc_100_fields_expands_without_panicking() {
        let mut m = serde_json::Map::new();
        for i in 0..120 {
            m.insert(format!("k{i}"), serde_json::json!(i));
        }
        let docs = vec![serde_json::Value::Object(m)];
        let mut e = set();
        e.insert("0".into());
        let lines = build_lines(&docs, &e);
        // 1 root + 120 field rows.
        assert_eq!(lines.len(), 121);
    }

    #[test]
    fn array_child_is_indexed() {
        let docs = vec![serde_json::json!({ "tags": ["a", "b"] })];
        let mut e = set();
        e.insert("0".into());
        e.insert("0.tags".into());
        let lines = build_lines(&docs, &e);
        // root + tags + [0] + [1] = 4.
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2].label, "[0]");
        assert_eq!(lines[2].summary, "\"a\"");
        assert_eq!(lines[3].label, "[1]");
    }

    #[test]
    fn parent_path_strips_trailing_key() {
        assert_eq!(parent_path("0.a.b"), Some("0.a".to_string()));
        assert_eq!(parent_path("0.a"), Some("0".to_string()));
        assert_eq!(parent_path("0"), None);
    }

    #[test]
    fn parent_path_strips_array_index() {
        assert_eq!(parent_path("0.tags[3]"), Some("0.tags".to_string()));
        assert_eq!(parent_path("2[0]"), Some("2".to_string()));
    }

    #[test]
    fn compute_scroll_keeps_cursor_visible() {
        // Cursor near top → no scroll.
        assert_eq!(compute_scroll(2, 100, 10), 0);
        // Cursor deep → scroll so cursor is roughly in the middle.
        assert_eq!(compute_scroll(50, 100, 10), 45);
        // Cursor at end → scroll clamps.
        assert_eq!(compute_scroll(99, 100, 10), 90);
    }

    #[test]
    fn compute_scroll_when_total_fits() {
        assert_eq!(compute_scroll(5, 8, 10), 0);
    }

    #[test]
    fn one_line_ellipsis_on_long_input() {
        let out = one_line(&"a".repeat(200), 40);
        assert_eq!(out.chars().count(), 40);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn doc_summary_prefers_id_field() {
        let v = serde_json::json!({ "name": "Alice", "_id": "abc" });
        let (icon, sum) = doc_summary(&v);
        assert_eq!(icon, '{');
        assert!(sum.contains("_id="));
    }
}
