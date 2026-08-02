//! App state — per-tab list of Lambda functions + a selection cursor.
//! Each tab is one filter view (`all` or `watched`). Loading runs on
//! tab activation + the auto-refresh tick + manual `r`.

use crate::config::{Config, Tab};
use crate::lambda::{self, Function};
use anyhow::Result;
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub watched: Vec<String>,
    pub region: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match t.kind.as_str() {
            "all" => Ok(Self {
                kind: "all".into(),
                watched: vec![],
                region,
            }),
            "watched" => {
                if t.watched.is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"watched\" requires `watched`", t.name);
                }
                Ok(Self {
                    kind: "watched".into(),
                    watched: t.watched.clone(),
                    region,
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

pub struct FunctionsTab {
    pub items: Vec<Function>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
}

impl FunctionsTab {
    fn empty() -> Self {
        FunctionsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: FunctionsTab,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t, cfg.region.as_deref())?;
            tabs.push(TabState {
                name: t.name.clone(),
                data: FunctionsTab::empty(),
                spec,
            });
        }
        let mut app = App {
            cfg,
            tabs,
            active_tab: 0,
            status: String::new(),
        };
        app.refresh_active();
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            if self.tabs[idx].data.items.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.data.items.is_empty() {
            return;
        }
        let n = tab.data.items.len() as isize;
        let cur = tab.data.selected as isize;
        let next = (cur + delta).clamp(0, n - 1);
        tab.data.selected = next as usize;
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;

        // Synchronous shell-out — v0.1 keeps it simple. The AWS CLI
        // is fast for typical function counts (≤200); if a region
        // has thousands of functions this would warrant a thread.
        let result: Result<Vec<Function>> = match spec.kind.as_str() {
            "all" => lambda::list_functions(spec.region.as_deref()),
            "watched" => {
                let mut out = Vec::with_capacity(spec.watched.len());
                let mut errs = Vec::new();
                for fn_name in &spec.watched {
                    match lambda::get_function(fn_name, spec.region.as_deref()) {
                        Ok(f) => out.push(f),
                        Err(e) => errs.push(format!("{fn_name}: {e}")),
                    }
                }
                if out.is_empty() && !errs.is_empty() {
                    Err(anyhow::anyhow!("{}", errs.join("; ")))
                } else {
                    Ok(out)
                }
            }
            _ => unreachable!("validated in TabSpec::resolve"),
        };

        let t = &mut self.tabs[idx];
        t.data.loading = false;
        match result {
            Ok(items) => {
                let count = items.len();
                t.data.items = items;
                t.data.selected = t.data.selected.min(count.saturating_sub(1));
                t.data.last_loaded = Some(Instant::now());
                t.data.last_error = None;
                self.status = format!("{name}: {count} functions");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Auto-refresh tick — called from the main loop. Refreshes the
    /// active tab if it hasn't been loaded for at least
    /// `cfg.refresh_interval_secs` seconds. Returns true if a
    /// refresh ran (the UI should redraw).
    pub fn tick(&mut self) -> bool {
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return false;
        }
        let idx = self.active_tab;
        let stale = match self.tabs[idx].data.last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].data.loading {
            self.refresh_active();
            true
        } else {
            false
        }
    }

    /// No-op kept for blit/event-loop parity with sibling shape.
    pub fn drain(&mut self) -> bool {
        false
    }

    pub fn focused_function(&self) -> Option<&Function> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    /// `o` — open the Lambda console URL for the focused function.
    pub fn open_console(&mut self) {
        let Some(fun) = self.focused_function() else {
            self.status = "no function under cursor".into();
            return;
        };
        let region = self.active().spec.region.as_deref().unwrap_or("us-east-1");
        let name = fun.function_name.clone();
        let url = format!(
            "https://{region}.console.aws.amazon.com/lambda/home?region={region}#/functions/{name}"
        );
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` — yank focused function's ARN to the clipboard.
    pub fn yank_arn(&mut self) {
        let Some(fun) = self.focused_function() else {
            self.status = "no function under cursor".into();
            return;
        };
        let arn = fun.function_arn.clone();
        match crate::clipboard::copy(&arn) {
            Ok(()) => self.status = format!("copied ARN ({} chars)", arn.len()),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `l` — hand off to `mnml-aws-cloudwatch-logs` scoped to the
    /// focused function's `/aws/lambda/<name>` log group. Spawns the
    /// sibling as a detached process in the standalone path; under
    /// blit-host the parent (mnml/tmnl) handles tab spawning, but
    /// v0.2 wires the cross-sibling handoff properly: spawns
    /// `mnml-aws-cloudwatch-logs --log-group /aws/lambda/<fn>
    /// --log-group-name <fn> [--region <r>]`. The sibling (v0.2+)
    /// recognises these CLI flags and builds a one-off single-tab
    /// session — bypassing the user's configured tabs entirely so
    /// the rest of their cloudwatch-logs setup stays intact.
    ///
    /// Falls back to a bare `mnml-aws-cloudwatch-logs` spawn if the
    /// sibling isn't installed; the status string flags the user to
    /// install it.
    pub fn tail_logs(&mut self) {
        let Some(fun) = self.focused_function() else {
            self.status = "no function under cursor".into();
            return;
        };
        let log_group = crate::lambda::log_group_for(&fun.function_name);
        let region = self.active().spec.region.clone();
        let fn_name = fun.function_name.clone();

        let mut cmd = Command::new("mnml-aws-cloudwatch-logs");
        cmd.args(["--log-group", &log_group, "--log-group-name", &fn_name]);
        if let Some(r) = &region {
            cmd.args(["--region", r]);
        }
        match cmd.spawn() {
            Ok(_) => {
                self.status = format!("tailing /aws/lambda/{fn_name}");
            }
            Err(e) => {
                self.status =
                    format!("spawn failed (install mnml-aws-cloudwatch-logs ≥ v0.2.0): {e}");
            }
        }
    }

    /// `L` — DLQ jump. When the focused function has a
    /// `DeadLetterConfig.TargetArn`, parse the ARN's service segment
    /// and spawn the matching sibling (sqs or sns). Mirrors the SNS
    /// subscription / EventBridge target handoff pattern.
    ///
    /// Today: spawns the sibling with no auto-scope to the specific
    /// queue / topic — the user still has to navigate to it.
    /// v0.x will pass a `--scope-arn` (or similar) so the sibling
    /// auto-focuses the right row.
    pub fn handoff_dlq(&mut self) {
        let Some(fun) = self.focused_function() else {
            self.status = "no function under cursor".into();
            return;
        };
        let Some(dlc) = &fun.dead_letter_config else {
            self.status = "no DLQ configured on this function".into();
            return;
        };
        let Some(arn) = dlc.target_arn.as_deref() else {
            self.status = "no DLQ configured on this function".into();
            return;
        };
        // ARN shape: `arn:aws:<service>:<region>:<account>:<resource>`.
        let segs: Vec<&str> = arn.split(':').collect();
        let service = segs.get(2).copied().unwrap_or("");
        let resource = segs.last().copied().unwrap_or("");

        let binary = match service {
            "sqs" => "mnml-aws-sqs",
            "sns" => "mnml-aws-sns",
            other => {
                self.status = format!("no sibling for `{other}` DLQ — supported: sqs, sns");
                return;
            }
        };

        match Command::new(binary).spawn() {
            Ok(_) => {
                self.status =
                    format!("launched {binary} — navigate to {resource} (auto-scope is v0.x)");
            }
            Err(e) => {
                self.status = format!("spawn {binary} failed (install it?): {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolve_uses_default_region() {
        let t = Tab {
            name: "x".into(),
            kind: "all".into(),
            watched: vec![],
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
        assert_eq!(spec.kind, "all");
    }

    #[test]
    fn tab_spec_rejects_watched_without_entries() {
        let t = Tab {
            name: "bad".into(),
            kind: "watched".into(),
            watched: vec![],
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }

    #[test]
    fn tab_spec_rejects_unknown_kind() {
        let t = Tab {
            name: "bad".into(),
            kind: "garbage".into(),
            watched: vec![],
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }
}
