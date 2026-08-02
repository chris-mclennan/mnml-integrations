//! Left panel — schema tree. Namespace rows expand/collapse; object
//! rows insert `namespace.name` into the editor on Enter.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, Focus, TreeLine};
use crate::ui::themed;

/// Recompute the flat visible-line list from the current schema
/// cache + expanded set. Called at the top of each frame so the
/// tree renderer + the keyboard navigator agree on what "line N" is.
pub fn rebuild_visible(app: &mut App) {
    let expanded = app.tree.expanded.clone();
    let mut lines = Vec::new();
    if let Some(c) = app.active_conn()
        && let Some(namespaces) = c.schema.namespaces.as_ref()
    {
        for ns in namespaces {
            lines.push(TreeLine::Namespace(ns.name.clone()));
            if expanded.contains(&ns.name)
                && let Some(objs) = c.schema.objects.get(&ns.name)
            {
                for o in objs {
                    lines.push(TreeLine::Object {
                        namespace: ns.name.clone(),
                        name: o.name.clone(),
                    });
                }
            }
        }
    }
    app.tree.visible = lines;
    if app.tree.selected >= app.tree.visible.len() {
        app.tree.selected = app.tree.visible.len().saturating_sub(1);
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::SchemaTree;
    let title_style = if focused {
        Style::default()
            .fg(themed(Color::Cyan))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(themed(Color::DarkGray))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" schema ", title_style));

    if app.active_conn().is_none() {
        let p = Paragraph::new("(no connection)")
            .style(Style::default().fg(themed(Color::DarkGray)))
            .block(block);
        f.render_widget(p, area);
        return;
    }

    // Empty tree — either not connected yet, or namespaces still
    // loading.
    if app.tree.visible.is_empty() {
        let hint = if app
            .active_conn()
            .map(|c| c.worker.is_none())
            .unwrap_or(true)
        {
            "(press Ctrl+Enter to open the connection)"
        } else {
            "(loading schema…)"
        };
        let p = Paragraph::new(hint)
            .style(Style::default().fg(themed(Color::DarkGray)))
            .block(block);
        f.render_widget(p, area);
        return;
    }

    let mut lines = Vec::with_capacity(app.tree.visible.len());
    for (i, line) in app.tree.visible.iter().enumerate() {
        let selected = i == app.tree.selected && focused;
        let text = match line {
            TreeLine::Namespace(ns) => {
                let expanded = app.tree.expanded.contains(ns);
                let arrow = if expanded { "▼" } else { "▶" };
                let marker = if selected { "▸ " } else { "  " };
                Line::from(vec![
                    Span::raw(marker),
                    Span::styled(
                        format!("{arrow} {ns}"),
                        Style::default()
                            .fg(themed(Color::Yellow))
                            .add_modifier(Modifier::BOLD),
                    ),
                ])
            }
            TreeLine::Object { name, .. } => {
                let marker = if selected { "▸  " } else { "   " };
                Line::from(vec![
                    Span::raw(marker),
                    Span::styled(
                        name.clone(),
                        if selected {
                            Style::default().add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(themed(Color::Gray))
                        },
                    ),
                ])
            }
        };
        lines.push(text);
    }
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}
