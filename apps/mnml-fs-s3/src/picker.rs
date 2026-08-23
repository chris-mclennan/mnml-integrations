//! Local-filesystem picker for the multi-file upload flow (#1048).
//!
//! Renders the contents of a directory, tracks which files the user
//! toggled with Space, and returns the resolved paths on Enter. The
//! picker is intentionally minimal — no filter, no sort options, no
//! grep — the caller can iterate more when the shape is proven.

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PickerEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// ISO-ish `YYYY-MM-DD` — good enough for the row.
    pub modified: String,
}

#[derive(Debug)]
pub struct FilePicker {
    pub cwd: PathBuf,
    pub entries: Vec<PickerEntry>,
    pub row: usize,
    pub selected: Vec<PathBuf>,
    pub error: Option<String>,
}

impl FilePicker {
    /// Open the picker rooted at `cwd`. On IO error, the picker still
    /// opens (empty) with the error surfaced — the user can go up.
    pub fn new(cwd: PathBuf) -> Self {
        let mut p = FilePicker {
            cwd,
            entries: Vec::new(),
            row: 0,
            selected: Vec::new(),
            error: None,
        };
        p.rescan();
        p
    }

    /// Re-read the current directory.
    pub fn rescan(&mut self) {
        match read_dir(&self.cwd) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
                if self.row >= self.entries.len() {
                    self.row = self.entries.len().saturating_sub(1);
                }
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e.to_string());
                self.row = 0;
            }
        }
    }

    pub fn focused(&self) -> Option<&PickerEntry> {
        self.entries.get(self.row)
    }

    pub fn move_row(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as isize;
        let next = (self.row as isize + delta).clamp(0, n - 1) as usize;
        self.row = next;
    }

    pub fn home(&mut self) {
        self.row = 0;
    }

    pub fn end(&mut self) {
        self.row = self.entries.len().saturating_sub(1);
    }

    /// Descend into a directory. No-op on files (Enter fires an upload
    /// on files — see `should_descend`).
    pub fn descend(&mut self) {
        let Some(e) = self.focused().cloned() else {
            return;
        };
        if !e.is_dir {
            return;
        }
        self.cwd = e.path.clone();
        self.row = 0;
        self.rescan();
    }

    /// Go up one directory. No-op at filesystem root.
    pub fn pop(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            self.cwd = parent.to_path_buf();
            self.row = 0;
            self.rescan();
        }
    }

    /// Toggle selection of the focused row. No-op on directories —
    /// uploading a directory recursively is a separate bigger task
    /// (would need `aws s3 cp --recursive` and different progress
    /// bookkeeping — deferred).
    pub fn toggle(&mut self) {
        let Some(e) = self.focused().cloned() else {
            return;
        };
        if e.is_dir {
            return;
        }
        if let Some(pos) = self.selected.iter().position(|p| p == &e.path) {
            self.selected.remove(pos);
        } else {
            self.selected.push(e.path);
        }
    }

    /// Select every file in the current dir (dirs skipped).
    pub fn select_all_files(&mut self) {
        for e in &self.entries {
            if !e.is_dir && !self.selected.iter().any(|p| p == &e.path) {
                self.selected.push(e.path.clone());
            }
        }
    }

    /// Clear all selections.
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// True iff the focused row's path is in the selected set. Used
    /// by tests today; kept public for the UI to key off if it ever
    /// needs a "focused-and-selected" style variation.
    #[allow(dead_code)]
    pub fn is_focused_selected(&self) -> bool {
        let Some(e) = self.focused() else {
            return false;
        };
        self.selected.iter().any(|p| p == &e.path)
    }

    /// Enter action — if focus is a directory, descend. If it's a
    /// file, return the paths to upload: the current selection (or,
    /// when selection is empty, the focused file). Returning None
    /// means "descended, don't fire upload".
    pub fn enter(&mut self) -> Option<Vec<PathBuf>> {
        let e = self.focused().cloned()?;
        if e.is_dir {
            self.descend();
            return None;
        }
        if self.selected.is_empty() {
            Some(vec![e.path])
        } else {
            Some(std::mem::take(&mut self.selected))
        }
    }
}

fn read_dir(dir: &Path) -> Result<Vec<PickerEntry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        // Skip dotfiles by convention — they're rarely upload targets,
        // and they otherwise dominate the listing on macOS.
        if file_name.starts_with('.') {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        let modified = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format_ymd(d.as_secs()))
            .unwrap_or_default();
        out.push(PickerEntry {
            path,
            name: file_name,
            is_dir,
            size,
            modified,
        });
    }
    // Dirs first, then files, each alphabetical.
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

/// Very small "epoch → YYYY-MM-DD" without a chrono dep. Precision
/// only needs a leaf-node date for the row.
fn format_ymd(secs: u64) -> String {
    // 86400s/day since 1970-01-01. Handles Gregorian for the range we
    // ever care about (this century).
    let days = (secs / 86_400) as i64;
    let (y, m, d) = ymd_from_epoch_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn ymd_from_epoch_days(mut days: i64) -> (i32, u32, u32) {
    // Algorithm from Howard Hinnant's date library. Correct for any
    // valid `days` value in the Gregorian range.
    days += 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = (days - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn ymd_computes_known_dates() {
        // Values verified via `date -j -f "%Y-%m-%d" YYYY-MM-DD +%s`
        // then divided by 86400.
        let (y, m, d) = ymd_from_epoch_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));
        let (y, m, d) = ymd_from_epoch_days(19_723);
        assert_eq!((y, m, d), (2024, 1, 1));
        let (y, m, d) = ymd_from_epoch_days(20_687);
        assert_eq!((y, m, d), (2026, 8, 22));
        // Leap-day boundary sanity.
        let (y, m, d) = ymd_from_epoch_days(19_782);
        assert_eq!((y, m, d), (2024, 2, 29));
        let (y, m, d) = ymd_from_epoch_days(19_783);
        assert_eq!((y, m, d), (2024, 3, 1));
    }

    #[test]
    fn picker_lists_and_toggles() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), b"hi").unwrap();
        fs::write(tmp.path().join("b.txt"), b"hello").unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();

        let mut p = FilePicker::new(tmp.path().to_path_buf());
        assert!(p.error.is_none());
        assert_eq!(p.entries.len(), 3);
        // Dirs first, then files, alpha within each.
        assert_eq!(p.entries[0].name, "sub");
        assert!(p.entries[0].is_dir);
        assert_eq!(p.entries[1].name, "a.txt");
        assert_eq!(p.entries[2].name, "b.txt");

        // Toggle on a directory row is a no-op.
        p.toggle();
        assert!(p.selected.is_empty());

        // Move down to a.txt and toggle.
        p.move_row(1);
        p.toggle();
        assert_eq!(p.selected.len(), 1);
        assert!(p.is_focused_selected());

        // Toggle again removes it.
        p.toggle();
        assert!(p.selected.is_empty());

        // Select-all-files picks both files, not the dir.
        p.select_all_files();
        assert_eq!(p.selected.len(), 2);
    }

    #[test]
    fn picker_enter_on_dir_descends() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub").join("nested.txt"), b"x").unwrap();

        let mut p = FilePicker::new(tmp.path().to_path_buf());
        assert_eq!(p.entries[0].name, "sub");
        assert!(p.enter().is_none()); // descended, no upload
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].name, "nested.txt");
    }

    #[test]
    fn picker_enter_on_file_returns_focused() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("only.txt"), b"x").unwrap();

        let mut p = FilePicker::new(tmp.path().to_path_buf());
        assert_eq!(p.entries.len(), 1);
        let paths = p.enter().unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("only.txt"));
    }

    #[test]
    fn picker_enter_returns_selection_when_nonempty() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a"), b"x").unwrap();
        fs::write(tmp.path().join("b"), b"x").unwrap();
        fs::write(tmp.path().join("c"), b"x").unwrap();

        let mut p = FilePicker::new(tmp.path().to_path_buf());
        p.select_all_files();
        p.move_row(0); // focus is on "a", but selection has all three
        let paths = p.enter().unwrap();
        assert_eq!(paths.len(), 3);
        // Selection cleared after enter.
        assert!(p.selected.is_empty());
    }

    #[test]
    fn picker_pop_climbs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();

        let mut p = FilePicker::new(tmp.path().to_path_buf());
        p.descend(); // into "sub"
        p.pop();
        assert_eq!(p.cwd, tmp.path());
    }

    #[test]
    fn picker_skips_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".hidden"), b"x").unwrap();
        fs::write(tmp.path().join("visible.txt"), b"x").unwrap();

        let p = FilePicker::new(tmp.path().to_path_buf());
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].name, "visible.txt");
    }
}
