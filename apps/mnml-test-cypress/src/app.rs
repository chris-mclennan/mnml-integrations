//! App state — owns the loaded TestReport, selection, filter mode.

use crate::cypress::{Spec, Test, TestReport, TestState};
use anyhow::Result;
use std::path::PathBuf;

/// What the UI displays right now — a flat sequence of test rows
/// (with spec headers) for the active filter.
#[derive(Debug, Clone)]
pub enum Row {
    SpecHeader {
        spec_idx: usize,
        passed: u32,
        failed: u32,
    },
    Test {
        spec_idx: usize,
        test_idx: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    FailedOnly,
}

pub struct App {
    pub source: PathBuf,
    pub report: TestReport,
    pub selected: usize,
    pub filter: Filter,
    pub status: String,
    /// Materialized row list for the current filter — re-built on
    /// load and when `filter` changes.
    pub rows: Vec<Row>,
}

impl App {
    pub fn new(source: PathBuf, report: TestReport) -> Result<Self> {
        let mut app = App {
            source,
            report,
            selected: 0,
            filter: Filter::FailedOnly,
            status: String::new(),
            rows: Vec::new(),
        };
        app.rebuild_rows();
        // FailedOnly default → fall back to All if there are no
        // failures (otherwise the user opens to an empty view).
        if app.rows.is_empty() {
            app.filter = Filter::All;
            app.rebuild_rows();
        }
        app.update_status();
        Ok(app)
    }

    pub fn reload(&mut self) {
        match crate::cypress::load(&self.source) {
            Ok(report) => {
                self.report = report;
                self.selected = 0;
                self.rebuild_rows();
                self.update_status();
                self.status = format!("reloaded · {}", self.status);
            }
            Err(e) => {
                self.status = format!("reload failed: {e}");
            }
        }
    }

    pub fn toggle_failed_only(&mut self) {
        self.filter = match self.filter {
            Filter::All => Filter::FailedOnly,
            Filter::FailedOnly => Filter::All,
        };
        self.selected = 0;
        self.rebuild_rows();
        self.update_status();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, n - 1) as usize;
        self.selected = next;
    }

    /// `y` — yank the spec file path (absolute) for the focused row.
    /// On a spec header, that's the spec's full_file. On a test
    /// row, same thing — copy the spec path so the user can `:e` it.
    pub fn yank_focused_spec_path(&mut self) {
        let Some(spec) = self.focused_spec() else {
            self.status = "no spec for this row".into();
            return;
        };
        let path = if spec.full_file.is_empty() {
            spec.file.clone()
        } else {
            spec.full_file.clone()
        };
        match crate::clipboard::copy(&path) {
            Ok(()) => self.status = format!("copied {path}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    pub fn focused_spec(&self) -> Option<&Spec> {
        let row = self.rows.get(self.selected)?;
        let idx = match row {
            Row::SpecHeader { spec_idx, .. } | Row::Test { spec_idx, .. } => *spec_idx,
        };
        self.report.specs.get(idx)
    }

    pub fn focused_test(&self) -> Option<&Test> {
        let row = self.rows.get(self.selected)?;
        let (spec_idx, test_idx) = match row {
            Row::Test { spec_idx, test_idx } => (*spec_idx, *test_idx),
            _ => return None,
        };
        self.report.specs.get(spec_idx)?.tests.get(test_idx)
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (si, spec) in self.report.specs.iter().enumerate() {
            let mut passed = 0u32;
            let mut failed = 0u32;
            for t in &spec.tests {
                match t.state {
                    TestState::Passed => passed += 1,
                    TestState::Failed => failed += 1,
                    _ => {}
                }
            }
            // Skip specs that have no matching rows under the
            // active filter — keeps the list tight when filtering.
            let any_match = spec.tests.iter().any(|t| self.test_passes_filter(t));
            if !any_match {
                continue;
            }
            self.rows.push(Row::SpecHeader {
                spec_idx: si,
                passed,
                failed,
            });
            for (ti, t) in spec.tests.iter().enumerate() {
                if self.test_passes_filter(t) {
                    self.rows.push(Row::Test {
                        spec_idx: si,
                        test_idx: ti,
                    });
                }
            }
        }
    }

    fn test_passes_filter(&self, t: &Test) -> bool {
        match self.filter {
            Filter::All => true,
            Filter::FailedOnly => t.state == TestState::Failed,
        }
    }

    fn update_status(&mut self) {
        let s = &self.report.stats;
        let filter = match self.filter {
            Filter::All => "all",
            Filter::FailedOnly => "failures only",
        };
        self.status = format!(
            "{}p / {}f / {}pending · {}/{} rows · filter: {}",
            s.passes,
            s.failures,
            s.pending,
            self.rows
                .iter()
                .filter(|r| matches!(r, Row::Test { .. }))
                .count(),
            s.tests,
            filter,
        );
    }
}
