//! Per-connection MRU query history on disk.
//!
//! Each connection gets its own newline-delimited file at
//! `~/.config/mnml-db/history/<id>.log`. v0.1 is append-and-tail —
//! no search, no de-duplication.
//!
//! `prev()` / `next()` / `is_empty()` are exposed for the recall-
//! chord (arrow-key history walk) that lands after v0.1's Ctrl+H
//! picker; kept live so tests don't rot.
#![allow(dead_code)]

use anyhow::Result;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::config::config_dir;

const MAX_ENTRIES: usize = 200;

pub struct History {
    path: PathBuf,
    entries: VecDeque<String>,
    /// Where the cursor is in the history (0 = "current unsent
    /// input", 1 = most recent past, ...).
    cursor: usize,
}

impl History {
    pub fn for_connection(id: &str) -> Result<Self> {
        let path = history_dir().join(format!("{id}.log"));
        let mut entries = VecDeque::new();
        if path.exists() {
            let f = std::fs::File::open(&path)?;
            for line in BufReader::new(f).lines().map_while(|l| l.ok()) {
                if !line.trim().is_empty() {
                    entries.push_back(line);
                }
            }
            while entries.len() > MAX_ENTRIES {
                entries.pop_front();
            }
        }
        Ok(Self {
            path,
            entries,
            cursor: 0,
        })
    }

    pub fn record(&mut self, statement: &str) -> Result<()> {
        let statement = statement.trim();
        if statement.is_empty() {
            return Ok(());
        }
        // Skip if identical to the most recent.
        if self.entries.back().is_some_and(|s| s == statement) {
            return Ok(());
        }
        self.entries.push_back(statement.to_string());
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
        std::fs::create_dir_all(self.path.parent().unwrap())?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{statement}")?;
        self.cursor = 0;
        Ok(())
    }

    pub fn entries(&self) -> &VecDeque<String> {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reset the recall cursor. Called when the editor content
    /// changes so subsequent `prev` starts at the tail again.
    pub fn reset_cursor(&mut self) {
        self.cursor = 0;
    }

    /// Move one entry into the past. Returns the entry to display
    /// (or `None` when already at the oldest entry).
    pub fn prev(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        if self.cursor < self.entries.len() {
            self.cursor += 1;
        }
        let idx = self.entries.len().checked_sub(self.cursor)?;
        self.entries.get(idx).cloned()
    }

    /// Move one entry toward "current input". Returns the entry at
    /// the new cursor, or `None` when back at 0 (caller restores
    /// the pre-recall buffer).
    pub fn next(&mut self) -> Option<String> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        if self.cursor == 0 {
            return None;
        }
        let idx = self.entries.len().checked_sub(self.cursor)?;
        self.entries.get(idx).cloned()
    }
}

fn history_dir() -> PathBuf {
    config_dir().join("history")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn hist_at(path: PathBuf) -> History {
        History {
            path,
            entries: VecDeque::new(),
            cursor: 0,
        }
    }

    #[test]
    fn record_appends_and_dedups_consecutive() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.log");
        let mut h = hist_at(path.clone());
        h.record("SELECT 1").unwrap();
        h.record("SELECT 1").unwrap();
        h.record("SELECT 2").unwrap();
        assert_eq!(h.entries.len(), 2);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "SELECT 1\nSELECT 2\n");
    }

    #[test]
    fn prev_next_walk_history() {
        let dir = TempDir::new().unwrap();
        let mut h = hist_at(dir.path().join("t.log"));
        h.record("a").unwrap();
        h.record("b").unwrap();
        h.record("c").unwrap();
        assert_eq!(h.prev(), Some("c".to_string()));
        assert_eq!(h.prev(), Some("b".to_string()));
        assert_eq!(h.prev(), Some("a".to_string()));
        // Already at oldest — stays there.
        assert_eq!(h.prev(), Some("a".to_string()));
        assert_eq!(h.next(), Some("b".to_string()));
        assert_eq!(h.next(), Some("c".to_string()));
        assert_eq!(h.next(), None);
    }

    #[test]
    fn empty_statement_not_recorded() {
        let dir = TempDir::new().unwrap();
        let mut h = hist_at(dir.path().join("t.log"));
        h.record("   ").unwrap();
        assert!(h.entries.is_empty());
    }
}
