//! Mochawesome JSON parser — the format Cypress emits when run with
//! the `mochawesome` reporter (`npx cypress run --reporter
//! mochawesome`). Single JSON file at
//! `cypress/results/mochawesome.json` (or wherever you configure it).
//!
//! Shape (compressed — only the fields we actually render):
//!
//! ```ignore
//! {
//!   "stats": { "tests", "passes", "failures", "pending", "duration" },
//!   "results": [
//!     {
//!       "fullFile": "/abs/path/login.cy.js",
//!       "file": "login.cy.js",
//!       "suites": [ {
//!         "title": "Login flow",
//!         "tests": [
//!           { "title": "logs in", "state": "passed", "duration": 1200 },
//!           { "title": "rejects bad pwd", "state": "failed",
//!             "err": { "message": "..", "stack": ".." } }
//!         ],
//!         "suites": [ /* nested */ ]
//!       } ],
//!       "tests": [ /* top-level tests in the file */ ]
//!     }
//!   ]
//! }
//! ```

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct TestReport {
    pub stats: Stats,
    pub specs: Vec<Spec>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub tests: u32,
    pub passes: u32,
    pub failures: u32,
    pub pending: u32,
    /// Total duration in milliseconds across all tests.
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Spec {
    /// Absolute path to the spec file (`/abs/path/login.cy.js`).
    pub full_file: String,
    /// Short path (`login.cy.js`).
    pub file: String,
    pub tests: Vec<Test>,
}

#[derive(Debug, Clone)]
pub struct Test {
    /// Combined suite path + title — `"Login flow > logs in"`.
    pub title: String,
    pub state: TestState,
    pub duration_ms: u64,
    /// Failure error message + stack (only set when state == Failed).
    pub error: Option<TestError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestState {
    Passed,
    Failed,
    Pending,
    Skipped,
    Unknown,
}

impl TestState {
    pub fn glyph(&self) -> &'static str {
        match self {
            TestState::Passed => "✓",
            TestState::Failed => "✗",
            TestState::Pending => "…",
            TestState::Skipped => "⊘",
            TestState::Unknown => "?",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestError {
    pub message: String,
    pub stack: Option<String>,
}

/// Parse a mochawesome JSON file from disk into the flat shape the
/// UI consumes.
pub fn load(path: &Path) -> Result<TestReport> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let raw: Raw = serde_json::from_str(&text).with_context(|| "parsing mochawesome JSON")?;
    Ok(transform(raw))
}

fn transform(raw: Raw) -> TestReport {
    let stats = Stats {
        tests: raw.stats.tests.unwrap_or(0),
        passes: raw.stats.passes.unwrap_or(0),
        failures: raw.stats.failures.unwrap_or(0),
        pending: raw.stats.pending.unwrap_or(0),
        duration_ms: raw.stats.duration.unwrap_or(0),
    };

    let mut specs: Vec<Spec> = Vec::with_capacity(raw.results.len());
    for r in raw.results {
        let mut tests: Vec<Test> = Vec::new();
        // Top-level (file-scoped) tests have no suite prefix.
        for t in r.tests.unwrap_or_default() {
            tests.push(flatten_test(&[], t));
        }
        // Recurse into the suite tree.
        for s in r.suites.unwrap_or_default() {
            collect_suite(&[], &s, &mut tests);
        }
        let full_file = r.full_file.unwrap_or_default();
        specs.push(Spec {
            file: r.file.unwrap_or_else(|| full_file.clone()),
            full_file,
            tests,
        });
    }
    TestReport { stats, specs }
}

fn collect_suite(path: &[String], s: &RawSuite, out: &mut Vec<Test>) {
    let mut new_path: Vec<String> = path.to_vec();
    if let Some(t) = &s.title
        && !t.is_empty()
    {
        new_path.push(t.clone());
    }
    for t in s.tests.iter().flatten() {
        out.push(flatten_test(&new_path, t.clone()));
    }
    for sub in s.suites.iter().flatten() {
        collect_suite(&new_path, sub, out);
    }
}

fn flatten_test(suite_path: &[String], t: RawTest) -> Test {
    let title = if suite_path.is_empty() {
        t.title.unwrap_or_default()
    } else {
        format!(
            "{} › {}",
            suite_path.join(" › "),
            t.title.unwrap_or_default()
        )
    };
    let state = match t.state.as_deref() {
        Some("passed") => TestState::Passed,
        Some("failed") => TestState::Failed,
        Some("pending") => TestState::Pending,
        Some("skipped") => TestState::Skipped,
        _ => TestState::Unknown,
    };
    let error = t.err.as_ref().and_then(|e| {
        let msg = e.message.clone().unwrap_or_default();
        if msg.is_empty() && e.stack.is_none() {
            None
        } else {
            Some(TestError {
                message: msg,
                stack: e.stack.clone(),
            })
        }
    });
    Test {
        title,
        state,
        duration_ms: t.duration.unwrap_or(0),
        error,
    }
}

// ─── Raw deserialization types ────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(default)]
    stats: RawStats,
    #[serde(default)]
    results: Vec<RawResult>,
}

#[derive(Debug, Default, Deserialize)]
struct RawStats {
    tests: Option<u32>,
    passes: Option<u32>,
    failures: Option<u32>,
    pending: Option<u32>,
    duration: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    #[serde(rename = "fullFile", default)]
    full_file: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    suites: Option<Vec<RawSuite>>,
    #[serde(default)]
    tests: Option<Vec<RawTest>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSuite {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tests: Option<Vec<RawTest>>,
    #[serde(default)]
    suites: Option<Vec<RawSuite>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    err: Option<RawErr>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawErr {
    #[serde(default)]
    message: Option<String>,
    #[serde(rename = "estack", alias = "stack", default)]
    stack: Option<String>,
}

/// Format a duration in ms as a short string: `1.2s`, `45ms`.
pub fn fmt_duration(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_mochawesome() {
        let json = r##"{
            "stats": { "tests": 3, "passes": 2, "failures": 1, "pending": 0, "duration": 4500 },
            "results": [
                {
                    "fullFile": "/abs/login.cy.js",
                    "file": "login.cy.js",
                    "suites": [
                        {
                            "title": "Login flow",
                            "tests": [
                                { "title": "logs in", "state": "passed", "duration": 1200 },
                                { "title": "redirects to dashboard", "state": "passed", "duration": 800 },
                                { "title": "rejects bad pwd", "state": "failed", "duration": 2500,
                                  "err": { "message": "expected dashboard, got login", "stack": "AssertionError: ..." } }
                            ]
                        }
                    ]
                }
            ]
        }"##;
        let raw: Raw = serde_json::from_str(json).unwrap();
        let report = transform(raw);
        assert_eq!(report.stats.tests, 3);
        assert_eq!(report.stats.failures, 1);
        assert_eq!(report.specs.len(), 1);
        assert_eq!(report.specs[0].tests.len(), 3);
        assert!(report.specs[0].tests[0].title.contains("Login flow"));
        assert_eq!(report.specs[0].tests[2].state, TestState::Failed);
        assert!(report.specs[0].tests[2].error.is_some());
    }

    #[test]
    fn handles_nested_suites() {
        let json = r##"{
            "stats": {},
            "results": [{
                "file": "x.cy.js",
                "suites": [{
                    "title": "Outer",
                    "suites": [{
                        "title": "Inner",
                        "tests": [{ "title": "deep test", "state": "passed", "duration": 50 }]
                    }]
                }]
            }]
        }"##;
        let raw: Raw = serde_json::from_str(json).unwrap();
        let report = transform(raw);
        let t = &report.specs[0].tests[0];
        assert_eq!(t.title, "Outer › Inner › deep test");
    }

    #[test]
    fn duration_format() {
        assert_eq!(fmt_duration(0), "0ms");
        assert_eq!(fmt_duration(500), "500ms");
        assert_eq!(fmt_duration(1200), "1.2s");
        assert_eq!(fmt_duration(45000), "45.0s");
    }
}
