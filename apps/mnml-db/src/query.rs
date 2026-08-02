//! Split multi-statement text into individual queries.
//!
//! `split_sql` is public for the palette command that runs a
//! whole editor as N distinct statements — used from `run_all` in
//! v0.2. Kept live in v0.1 so tests + palette-cmd wiring don't rot.
#![allow(dead_code)]
//!
//! Extremely small on purpose — v0.1 splits on top-level `;` while
//! respecting single-quoted, double-quoted, and dollar-tagged
//! strings ($tag$...$tag$). Redis command lines are usually one per
//! line, so `split_statements` there is really `split_on_newline`.

/// Split a SQL text into distinct statements. Preserves whitespace
/// inside statements; trims leading/trailing whitespace between them.
pub fn split_sql(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut dollar_tag: Option<String> = None;
    while let Some(c) = chars.next() {
        cur.push(c);
        match c {
            _ if dollar_tag.is_some() => {
                // Look ahead for the closing tag.
                let tag = dollar_tag.as_deref().unwrap().to_string();
                if cur.ends_with(&format!("${tag}$")) {
                    dollar_tag = None;
                }
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '$' if !in_single && !in_double => {
                // Look ahead for `$tag$` (or `$$`).
                let mut tag = String::new();
                let mut ahead = chars.clone();
                let mut ok = false;
                for c2 in ahead.by_ref() {
                    if c2 == '$' {
                        ok = true;
                        break;
                    }
                    if c2.is_ascii_alphanumeric() || c2 == '_' {
                        tag.push(c2);
                    } else {
                        break;
                    }
                }
                if ok {
                    // consume the tag + trailing `$`
                    for _ in 0..(tag.len() + 1) {
                        if let Some(c2) = chars.next() {
                            cur.push(c2);
                        }
                    }
                    dollar_tag = Some(tag);
                }
            }
            ';' if !in_single && !in_double && dollar_tag.is_none() => {
                // Statement boundary.
                let mut piece = cur.trim().to_string();
                if piece.ends_with(';') {
                    piece.pop();
                    piece = piece.trim().to_string();
                }
                if !piece.is_empty() {
                    out.push(piece);
                }
                cur.clear();
            }
            _ => {}
        }
    }
    let last = cur.trim().to_string();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

/// Return the statement whose text-range covers the byte-offset
/// `cursor` (or the last one if the cursor is past the end).
pub fn statement_at_cursor(text: &str, cursor: usize) -> String {
    // Simple: find the last `;` before cursor and the next `;` after.
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let after = &text[cursor..];
    let start = before.rfind(';').map(|i| i + 1).unwrap_or(0);
    let end = after.find(';').map(|i| cursor + i).unwrap_or(text.len());
    text[start..end].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_single_statement_no_semicolon() {
        assert_eq!(split_sql("SELECT 1"), vec!["SELECT 1"]);
    }

    #[test]
    fn split_two_statements() {
        assert_eq!(
            split_sql("SELECT 1; SELECT 2;"),
            vec!["SELECT 1", "SELECT 2"]
        );
    }

    #[test]
    fn split_preserves_semicolon_inside_single_quotes() {
        assert_eq!(
            split_sql("SELECT ';'; SELECT 1"),
            vec!["SELECT ';'", "SELECT 1"]
        );
    }

    #[test]
    fn split_preserves_semicolon_inside_dollar_tag() {
        assert_eq!(
            split_sql("SELECT $$a;b$$; SELECT 2"),
            vec!["SELECT $$a;b$$", "SELECT 2"]
        );
    }

    #[test]
    fn statement_at_cursor_returns_current_span() {
        let text = "SELECT 1; SELECT 2; SELECT 3";
        // Cursor inside "SELECT 2".
        let s = statement_at_cursor(text, 15);
        assert_eq!(s, "SELECT 2");
    }

    #[test]
    fn statement_at_cursor_end_of_text() {
        let text = "SELECT 1; SELECT 2";
        let s = statement_at_cursor(text, text.len());
        assert_eq!(s, "SELECT 2");
    }
}
