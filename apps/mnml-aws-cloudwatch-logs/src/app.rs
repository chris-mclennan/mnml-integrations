//! App state — per-tab live log tail, status string. Each `[[tabs]]`
//! entry is one log group + optional stream filter; the tab spawns
//! `aws logs tail --follow` on first activation and keeps the child
//! running until the tab is closed.

use crate::config::{Config, Tab};
use crate::log_tail::{LogTailEvent, LogTailPane};
use anyhow::Result;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub region: Option<String>,
    pub log_group: String,
    pub log_stream: Option<String>,
    /// Optional `--filter-pattern` (CloudWatch Logs filter syntax).
    /// Passed straight through to the `aws logs tail` call.
    pub filter: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        if t.log_group.trim().is_empty() {
            anyhow::bail!("tab `{}`: log_group is required", t.name);
        }
        Ok(Self {
            region,
            log_group: t.log_group.clone(),
            log_stream: t.log_stream.clone(),
            filter: t.filter.clone(),
        })
    }
}

pub struct LogsTab {
    pub pane: Option<LogTailPane>,
    pub pending: Option<Receiver<LogTailEvent>>,
    pub last_error: Option<String>,
}

impl LogsTab {
    fn empty() -> Self {
        LogsTab {
            pane: None,
            pending: None,
            last_error: None,
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: LogsTab,
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
                data: LogsTab::empty(),
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
            self.refresh_active();
        }
    }

    /// Scroll the focused tab's log buffer. Negative deltas scroll
    /// up (pausing auto-scroll); positive deltas scroll down (and
    /// jump back to live-tail when reaching bottom).
    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if let Some(p) = tab.data.pane.as_mut() {
            if delta < 0 {
                let n = (-delta) as usize;
                if p.scroll == usize::MAX {
                    p.scroll = p.lines.len().saturating_sub(1);
                }
                p.scroll = p.scroll.saturating_sub(n);
            } else {
                let n = delta as usize;
                let total = p.lines.len();
                if p.scroll == usize::MAX || p.scroll.saturating_add(n) >= total {
                    p.scroll = usize::MAX;
                } else {
                    p.scroll += n;
                }
            }
        }
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        // Spawn only if not already running.
        let needs_spawn = self.tabs[idx].data.pane.is_none();
        if needs_spawn {
            self.status = format!("starting {name}…");
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let res = LogTailPane::spawn_with_filter(
                spec.log_group.clone(),
                spec.log_stream.clone(),
                spec.filter.clone(),
                spec.region.clone(),
                cwd,
            );
            let t = &mut self.tabs[idx];
            match res {
                Ok((pane, rx)) => {
                    t.data.pane = Some(pane);
                    t.data.pending = Some(rx);
                    t.data.last_error = None;
                }
                Err(e) => {
                    t.data.last_error = Some(e.clone());
                    self.status = format!("error: {e}");
                }
            }
        }
    }

    /// Drain background channels — call from the main loop.
    pub fn drain(&mut self) -> bool {
        let mut any = false;
        for tab in self.tabs.iter_mut() {
            let Some(rx) = tab.data.pending.take() else {
                continue;
            };
            let mut still_open = true;
            loop {
                match rx.try_recv() {
                    Ok(LogTailEvent::Line(text)) => {
                        if let Some(p) = tab.data.pane.as_mut() {
                            use crate::log_tail::{LineSeverity, LogLine};
                            let severity = LineSeverity::classify(&text);
                            p.lines.push(LogLine { text, severity });
                            if p.lines.len() > 5000 {
                                let drop = p.lines.len() - 5000;
                                p.lines.drain(0..drop);
                            }
                            any = true;
                        }
                    }
                    Ok(LogTailEvent::Failed(e)) => {
                        tab.data.last_error = Some(e.clone());
                        self.status = format!("error: {e}");
                        still_open = false;
                        any = true;
                    }
                    Ok(LogTailEvent::Exited(_)) => {
                        still_open = false;
                        any = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        still_open = false;
                        break;
                    }
                }
            }
            if still_open {
                tab.data.pending = Some(rx);
            }
        }
        any
    }

    /// `o` — open the CloudWatch console URL for the active tab's
    /// log group / stream in the OS browser.
    pub fn open_console(&mut self) {
        let tab = self.active();
        let region = tab.spec.region.as_deref().unwrap_or("us-east-1");
        let group = urlencode(&tab.spec.log_group);
        let url = if let Some(stream) = &tab.spec.log_stream {
            let stream_enc = urlencode(stream);
            format!(
                "https://{region}.console.aws.amazon.com/cloudwatch/home?region={region}#logsV2:log-groups/log-group/{group}/log-events/{stream_enc}"
            )
        } else {
            format!(
                "https://{region}.console.aws.amazon.com/cloudwatch/home?region={region}#logsV2:log-groups/log-group/{group}"
            )
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` — yank the focused log line. Picks the visible-bottom
    /// line (the one the user is most likely looking at when they
    /// hit `y`); on a scrolled view, it's the line under the
    /// current scroll position.
    pub fn yank_focused_line(&mut self) {
        let tab = self.active();
        let Some(p) = tab.data.pane.as_ref() else {
            self.status = "no log buffer for this tab".to_string();
            return;
        };
        let row = if p.scroll == usize::MAX {
            p.lines.len().saturating_sub(1)
        } else {
            p.scroll
        };
        let Some(line) = p.lines.get(row) else {
            self.status = "no line under cursor".to_string();
            return;
        };
        match crate::clipboard::copy(&line.text) {
            Ok(()) => self.status = format!("copied 1 line ({} chars)", line.text.len()),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b'/' => out.push_str("$252F"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolve_uses_default_region() {
        let t = Tab {
            name: "x".into(),
            log_group: "/aws/lambda/x".into(),
            log_stream: None,
            region: None,
            filter: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_empty_log_group() {
        let t = Tab {
            name: "bad".into(),
            log_group: "".into(),
            log_stream: None,
            region: None,
            filter: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }

    #[test]
    fn urlencode_escapes_slash_correctly() {
        let s = urlencode("/aws/lambda/my-func");
        assert!(s.contains("$252F"));
        assert!(!s.contains('/'));
    }
}
