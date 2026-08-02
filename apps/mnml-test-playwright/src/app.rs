//! App state — wraps the lifted `TracePane` + status string. v0.1
//! supports one trace at a time (CLI arg or, in blit-host mode, the
//! path that was passed at launch).

use crate::trace::parse_trace_zip;
use crate::trace_pane::TracePane;
use anyhow::Result;
use std::path::PathBuf;

pub struct App {
    pub pane: TracePane,
    pub status: String,
}

impl App {
    pub fn open(path: PathBuf) -> Result<Self> {
        let events =
            parse_trace_zip(&path).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("trace")
            .to_string();
        let pane = TracePane::new(title, path, events);
        Ok(App {
            pane,
            status: String::new(),
        })
    }

    pub fn refresh(&mut self) {
        let path = self.pane.path.clone();
        match parse_trace_zip(&path) {
            Ok(events) => {
                let title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("trace")
                    .to_string();
                self.pane = TracePane::new(title, path, events);
                self.status = "trace reloaded".to_string();
            }
            Err(e) => self.status = format!("reload failed: {e}"),
        }
    }
}
