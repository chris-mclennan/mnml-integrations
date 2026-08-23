//! App state — per-tab list of SQS queues + a selection cursor.
//! Attributes are loaded lazily when a queue gets focus (similar to
//! the EventBridge targets pattern), so an account with hundreds of
//! queues opens fast — only the focused row pays the per-queue
//! `get-queue-attributes` cost.

use crate::config::{Config, Tab};
use crate::sqs::{self, Queue};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub prefix: Option<String>,
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
                prefix: None,
                region,
            }),
            "prefix" => {
                let prefix = t.prefix.clone().unwrap_or_default();
                if prefix.trim().is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"prefix\" requires `prefix`", t.name);
                }
                Ok(Self {
                    kind: "prefix".into(),
                    prefix: Some(prefix),
                    region,
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

pub struct ItemsTab {
    pub queues: Vec<Queue>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            queues: Vec::new(),
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
    pub data: ItemsTab,
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
                data: ItemsTab::empty(),
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
        app.ensure_focused_loaded();
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
            if self.tabs[idx].data.queues.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
            self.ensure_focused_loaded();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        {
            let tab = self.active_mut();
            if tab.data.queues.is_empty() {
                return;
            }
            let n = tab.data.queues.len() as isize;
            let cur = tab.data.selected as isize;
            let next = (cur + delta).clamp(0, n - 1);
            tab.data.selected = next as usize;
        }
        self.ensure_focused_loaded();
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;

        let prefix = match spec.kind.as_str() {
            "prefix" => spec.prefix.as_deref(),
            _ => None,
        };
        let result = sqs::list_queues(prefix, spec.region.as_deref());
        let t = &mut self.tabs[idx];
        t.data.loading = false;
        match result {
            Ok(urls) => {
                let count = urls.len();
                t.data.queues = urls
                    .into_iter()
                    .map(|url| Queue {
                        url,
                        attributes: None,
                        redrive_sources: vec![],
                        dlq_stats: None,
                    })
                    .collect();
                t.data.selected = t.data.selected.min(count.saturating_sub(1));
                t.data.last_loaded = Some(Instant::now());
                t.data.last_error = None;
                self.status = format!("{name}: {count} queues");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Fetch attributes for the focused queue if we haven't already.
    /// No-op for queues whose attributes are already cached.
    pub fn ensure_focused_loaded(&mut self) {
        let idx = self.active_tab;
        let Some(q_idx) = self
            .tabs
            .get(idx)
            .map(|t| t.data.selected)
            .filter(|&s| s < self.tabs[idx].data.queues.len())
        else {
            return;
        };
        if self.tabs[idx].data.queues[q_idx].attributes.is_some() {
            return;
        }
        let url = self.tabs[idx].data.queues[q_idx].url.clone();
        let region = self.tabs[idx].spec.region.clone();
        match sqs::get_attributes(&url, region.as_deref()) {
            Ok(attrs) => {
                self.tabs[idx].data.queues[q_idx].attributes = Some(attrs);
            }
            Err(e) => {
                self.status = format!("attrs: {e}");
            }
        }
    }

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

    pub fn focused_queue(&self) -> Option<&Queue> {
        let t = self.active();
        t.data.queues.get(t.data.selected)
    }

    pub fn open_console(&mut self) {
        let Some(q) = self.focused_queue() else {
            self.status = "no queue under cursor".into();
            return;
        };
        let region = self.active().spec.region.as_deref().unwrap_or("us-east-1");
        // Derive name from URL — works for the console URL.
        let name = q.name();
        let url = format!(
            "https://{region}.console.aws.amazon.com/sqs/v3/home?region={region}#/queues/{}",
            urlencode_path(&q.url)
        );
        let _ = name;
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    pub fn yank_url(&mut self) {
        let Some(q) = self.focused_queue() else {
            self.status = "no queue under cursor".into();
            return;
        };
        let url = q.url.clone();
        match crate::clipboard::copy(&url) {
            Ok(()) => self.status = format!("copied queue URL: {url}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    pub fn yank_arn(&mut self) {
        let Some(q) = self.focused_queue() else {
            self.status = "no queue under cursor".into();
            return;
        };
        let Some(arn) = q.attributes.as_ref().and_then(|a| a.arn()) else {
            self.status =
                "ARN not loaded yet — wait for attribute fetch to finish then retry".into();
            return;
        };
        let arn = arn.to_string();
        match crate::clipboard::copy(&arn) {
            Ok(()) => self.status = format!("copied ARN: {arn}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `A` — fetch attributes for every queue in the current tab, then
    /// run the redrive-source correlation pass. Expensive (N
    /// `get-queue-attributes` calls) — so it's an explicit action,
    /// not part of normal navigation. Reports progress via the status
    /// line.
    pub fn load_all_attributes(&mut self) {
        let idx = self.active_tab;
        let n = self.tabs[idx].data.queues.len();
        if n == 0 {
            self.status = "no queues to load".into();
            return;
        }
        let region = self.tabs[idx].spec.region.clone();
        let mut loaded_now = 0usize;
        for q_idx in 0..n {
            if self.tabs[idx].data.queues[q_idx].attributes.is_some() {
                continue;
            }
            let url = self.tabs[idx].data.queues[q_idx].url.clone();
            match sqs::get_attributes(&url, region.as_deref()) {
                Ok(attrs) => {
                    self.tabs[idx].data.queues[q_idx].attributes = Some(attrs);
                    loaded_now += 1;
                }
                Err(e) => {
                    // Surface the error but keep going — partial knowledge
                    // is better than nothing for the correlation pass.
                    self.status = format!("attrs error on {url}: {e}");
                }
            }
        }
        self.recompute_redrive_sources();
        let referenced_dlqs = self.tabs[idx]
            .data
            .queues
            .iter()
            .filter(|q| !q.redrive_sources.is_empty())
            .count();
        self.status =
            format!("loaded {loaded_now} new · {n} total · {referenced_dlqs} queues are DLQs");
    }

    /// `L` — jump the cursor to the focused queue's DLQ relationship.
    /// Two directions:
    ///   * Focused queue has a `RedrivePolicy` → jump to the queue it
    ///     names as its DLQ.
    ///   * Focused queue has `redrive_sources` (other queues point at
    ///     THIS one as their DLQ) → jump to the first source.
    ///
    /// Both target queues must be loaded in the current tab; otherwise
    /// the status line explains and selection stays put. Prereq: `A`
    /// (load all attributes) has run at least once so ARNs +
    /// `redrive_sources` are populated.
    pub fn jump_to_dlq(&mut self) {
        let idx = self.active_tab;
        let sel = self.tabs[idx].data.selected;
        let Some(q) = self.tabs[idx].data.queues.get(sel) else {
            self.status = "no queue under cursor".into();
            return;
        };
        // Direction 1: focused queue → its DLQ via RedrivePolicy.
        let dlq_target: Option<String> = q
            .attributes
            .as_ref()
            .and_then(|a| a.redrive_policy())
            .and_then(sqs::dlq_target_arn);
        if let Some(target_arn) = dlq_target {
            // Find the queue with this ARN in the current tab's list.
            let found = self.tabs[idx].data.queues.iter().position(|cand| {
                cand.attributes
                    .as_ref()
                    .and_then(|a| a.arn())
                    .map(|arn| arn == target_arn)
                    .unwrap_or(false)
            });
            match found {
                Some(target_idx) => {
                    self.tabs[idx].data.selected = target_idx;
                    let target_name = self.tabs[idx].data.queues[target_idx].name().to_string();
                    self.status = format!("jumped to DLQ: {target_name}");
                }
                None => {
                    // ARN known but the target queue isn't in this tab.
                    // Could be a DLQ in a different region / account, or
                    // simply not loaded yet (no `A`).
                    self.status = format!(
                        "DLQ {target_arn} not loaded in this tab — switch to All Queues + press A"
                    );
                }
            }
            return;
        }
        // Direction 2: focused queue IS a DLQ → jump to first source.
        if let Some(source_name) = q.redrive_sources.first() {
            let source_name = source_name.clone();
            let found = self.tabs[idx]
                .data
                .queues
                .iter()
                .position(|cand| cand.name() == source_name);
            match found {
                Some(source_idx) => {
                    self.tabs[idx].data.selected = source_idx;
                    self.status = format!("jumped to DLQ source: {source_name}");
                }
                None => {
                    self.status = format!("source {source_name} not in tab — press A first");
                }
            }
            return;
        }
        // No RedrivePolicy and no recorded sources.
        self.status = if q.attributes.is_some() {
            "no DLQ relationship — queue has no RedrivePolicy and isn't referenced as one".into()
        } else {
            "attributes not loaded — press A first".into()
        };
    }

    /// Walk all loaded queues' RedrivePolicy fields and populate each
    /// queue's `redrive_sources` with the names of queues that point
    /// AT it as their DLQ, AND (#1009) each queue-with-a-DLQ's
    /// `dlq_stats` with the message counts on its paired DLQ so the
    /// side-by-side `main:N · dlq:M` chip renders without a second
    /// lookup per row. Idempotent — clears and rebuilds both fields.
    pub fn recompute_redrive_sources(&mut self) {
        let idx = self.active_tab;
        // First pass: build target_arn → [source_name] map, and
        // arn → (visible, in_flight) so the second pass can populate
        // dlq_stats without re-scanning.
        let mut by_target: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut counts_by_arn: std::collections::HashMap<String, sqs::DlqStats> =
            std::collections::HashMap::new();
        for q in &self.tabs[idx].data.queues {
            let Some(attrs) = &q.attributes else {
                continue;
            };
            if let Some(arn) = attrs.arn() {
                counts_by_arn.insert(
                    arn.to_string(),
                    sqs::DlqStats {
                        visible: attrs.approximate_messages().unwrap_or(0),
                        in_flight: attrs.approximate_messages_not_visible().unwrap_or(0),
                    },
                );
            }
            let Some(policy) = attrs.redrive_policy() else {
                continue;
            };
            let Some(target_arn) = sqs::dlq_target_arn(policy) else {
                continue;
            };
            by_target
                .entry(target_arn)
                .or_default()
                .push(q.name().to_string());
        }
        // Second pass: for each queue, look up (a) its OWN ARN in
        // by_target → sources, and (b) its DLQ target ARN in
        // counts_by_arn → dlq_stats.
        for q in &mut self.tabs[idx].data.queues {
            let Some(attrs) = &q.attributes else {
                q.redrive_sources.clear();
                q.dlq_stats = None;
                continue;
            };
            let Some(arn) = attrs.arn() else {
                q.redrive_sources.clear();
                q.dlq_stats = None;
                continue;
            };
            q.redrive_sources = by_target.get(arn).cloned().unwrap_or_default();
            q.redrive_sources.sort();

            // DLQ stats — only when this queue has a DLQ AND the
            // DLQ is loaded (in this tab, in this region). A DLQ
            // in a different region / account leaves dlq_stats
            // `None`, which the renderer treats as "hide the chip"
            // rather than "zero" (see secondary_label).
            q.dlq_stats = attrs
                .redrive_policy()
                .and_then(sqs::dlq_target_arn)
                .and_then(|target| counts_by_arn.get(&target).copied());
        }
    }
}

/// URL-encode a string for use as a path segment (queue URL goes
/// inside the console's hash fragment).
fn urlencode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                out.push(b as char);
            }
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
            kind: "all".into(),
            prefix: None,
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_prefix_without_value() {
        let t = Tab {
            name: "bad".into(),
            kind: "prefix".into(),
            prefix: None,
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }

    // #1009 — recompute_redrive_sources populates both directions:
    // sources for DLQs and dlq_stats for queues that have a DLQ.
    #[test]
    fn recompute_populates_dlq_stats_across_queues() {
        use crate::sqs::{DlqStats, QueueAttributes};
        use std::collections::HashMap;
        let cfg = Config {
            region: Some("us-east-1".into()),
            refresh_interval_secs: 0,
            tabs: vec![Tab {
                name: "all".into(),
                kind: "all".into(),
                prefix: None,
                region: None,
            }],
        };
        // Bypass network by constructing App with cfg + hand-loaded
        // queues. We don't call `new()` because that would call the
        // aws CLI.
        let mut app = App {
            cfg,
            tabs: vec![TabState {
                name: "all".into(),
                spec: TabSpec::resolve(
                    &Tab {
                        name: "all".into(),
                        kind: "all".into(),
                        prefix: None,
                        region: None,
                    },
                    Some("us-east-1"),
                )
                .unwrap(),
                data: ItemsTab::empty(),
            }],
            active_tab: 0,
            status: String::new(),
        };
        // main queue → DLQ arn:dlq. Loaded main has 5432 visible.
        let main_attrs = QueueAttributes::from_map(HashMap::from([
            (
                "QueueArn".to_string(),
                "arn:aws:sqs:us-east-1:1:main".to_string(),
            ),
            (
                "ApproximateNumberOfMessages".to_string(),
                "5432".to_string(),
            ),
            (
                "RedrivePolicy".to_string(),
                r#"{"deadLetterTargetArn":"arn:aws:sqs:us-east-1:1:dlq","maxReceiveCount":3}"#
                    .to_string(),
            ),
        ]));
        // dlq queue with 12 visible + 2 in-flight — this is what
        // the chip should surface.
        let dlq_attrs = QueueAttributes::from_map(HashMap::from([
            (
                "QueueArn".to_string(),
                "arn:aws:sqs:us-east-1:1:dlq".to_string(),
            ),
            ("ApproximateNumberOfMessages".to_string(), "12".to_string()),
            (
                "ApproximateNumberOfMessagesNotVisible".to_string(),
                "2".to_string(),
            ),
        ]));
        app.tabs[0].data.queues = vec![
            Queue {
                url: "https://sqs.us-east-1.amazonaws.com/1/main".into(),
                attributes: Some(main_attrs),
                redrive_sources: vec![],
                dlq_stats: None,
            },
            Queue {
                url: "https://sqs.us-east-1.amazonaws.com/1/dlq".into(),
                attributes: Some(dlq_attrs),
                redrive_sources: vec![],
                dlq_stats: None,
            },
        ];
        app.recompute_redrive_sources();
        // main → learned dlq_stats.
        assert_eq!(
            app.tabs[0].data.queues[0].dlq_stats,
            Some(DlqStats {
                visible: 12,
                in_flight: 2
            })
        );
        // dlq → learned it's a source for `main`.
        assert_eq!(
            app.tabs[0].data.queues[1].redrive_sources,
            vec!["main".to_string()]
        );
        // dlq itself doesn't have a DLQ, so no dlq_stats on it.
        assert!(app.tabs[0].data.queues[1].dlq_stats.is_none());
    }
}
