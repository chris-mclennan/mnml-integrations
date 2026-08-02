//! App state — per-tab list of SNS items (topics OR subscriptions) +
//! a selection cursor. Topic attributes are loaded lazily on focus
//! (similar to the EventBridge targets / SQS queue patterns).

use crate::config::{Config, Tab};
use crate::sns::{self, Subscription, Topic};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub topic_arn: Option<String>,
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
            "topics" => Ok(Self {
                kind: "topics".into(),
                topic_arn: None,
                prefix: t.prefix.clone(),
                region,
            }),
            "subscriptions" => {
                let arn = t.topic_arn.clone().unwrap_or_default();
                if arn.trim().is_empty() {
                    anyhow::bail!(
                        "tab `{}`: kind=\"subscriptions\" requires `topic_arn`",
                        t.name
                    );
                }
                Ok(Self {
                    kind: "subscriptions".into(),
                    topic_arn: Some(arn),
                    prefix: None,
                    region,
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Topic(Topic),
    Subscription(Subscription),
}

impl Item {
    pub fn primary_label(&self) -> String {
        match self {
            Item::Topic(t) => t.name().to_string(),
            Item::Subscription(s) => s.protocol.clone().unwrap_or_else(|| "—".into()),
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Topic(t) => t.secondary_label(),
            Item::Subscription(s) => {
                let endpoint = s.endpoint_short();
                if s.is_pending_confirmation() {
                    format!("{endpoint}  ⚠ pending confirmation")
                } else {
                    endpoint.to_string()
                }
            }
        }
    }
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
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
    pub data: ItemsTab,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    /// When `Some`, the user is composing a publish-test message
    /// against the focused topic. Keys go to `publish_input` until
    /// Enter (commit) or Esc (cancel). Only valid when focus is a
    /// topic (subscriptions can't be published to).
    pub publish_editing: Option<String>,
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
            publish_editing: None,
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
            if self.tabs[idx].data.items.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
            self.ensure_focused_loaded();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        {
            let tab = self.active_mut();
            if tab.data.items.is_empty() {
                return;
            }
            let n = tab.data.items.len() as isize;
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

        let result: Result<Vec<Item>> = match spec.kind.as_str() {
            "topics" => {
                sns::list_topics(spec.prefix.as_deref(), spec.region.as_deref()).map(|arns| {
                    arns.into_iter()
                        .map(|arn| {
                            Item::Topic(Topic {
                                arn,
                                attributes: None,
                            })
                        })
                        .collect()
                })
            }
            "subscriptions" => {
                let arn = spec
                    .topic_arn
                    .as_deref()
                    .expect("subscriptions tab requires topic_arn (validated)");
                sns::list_subscriptions_by_topic(arn, spec.region.as_deref())
                    .map(|subs| subs.into_iter().map(Item::Subscription).collect())
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
                let kind_label = match spec.kind.as_str() {
                    "topics" => "topics",
                    "subscriptions" => "subscriptions",
                    _ => "items",
                };
                self.status = format!("{name}: {count} {kind_label}");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Fetch attributes for the focused topic if we haven't already.
    /// No-op for subscriptions (they came from list-subscriptions with
    /// full detail already) and for topics that are cached.
    pub fn ensure_focused_loaded(&mut self) {
        let idx = self.active_tab;
        let Some(t_idx) = self
            .tabs
            .get(idx)
            .map(|t| t.data.selected)
            .filter(|&s| s < self.tabs[idx].data.items.len())
        else {
            return;
        };
        let needs_load = match &self.tabs[idx].data.items[t_idx] {
            Item::Topic(t) => t.attributes.is_none(),
            Item::Subscription(_) => false,
        };
        if !needs_load {
            return;
        }
        let Item::Topic(topic) = &self.tabs[idx].data.items[t_idx] else {
            return;
        };
        let arn = topic.arn.clone();
        let region = self.tabs[idx].spec.region.clone();
        match sns::get_topic_attributes(&arn, region.as_deref()) {
            Ok(attrs) => {
                if let Item::Topic(t) = &mut self.tabs[idx].data.items[t_idx] {
                    t.attributes = Some(attrs);
                }
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

    pub fn drain(&mut self) -> bool {
        false
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    pub fn open_console(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let region = self.active().spec.region.as_deref().unwrap_or("us-east-1");
        let url = match item {
            Item::Topic(t) => format!(
                "https://{region}.console.aws.amazon.com/sns/v3/home?region={region}#/topic/{}",
                urlencode_path(&t.arn)
            ),
            Item::Subscription(s) => {
                let topic_arn = s.topic_arn.as_deref().unwrap_or("");
                format!(
                    "https://{region}.console.aws.amazon.com/sns/v3/home?region={region}#/topic/{}",
                    urlencode_path(topic_arn)
                )
            }
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` — yank focused item's ARN (topic ARN or subscription ARN).
    pub fn yank_arn(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let arn = match item {
            Item::Topic(t) => t.arn.clone(),
            Item::Subscription(s) => {
                if s.is_pending_confirmation() {
                    self.status = "subscription is pending — no ARN yet".into();
                    return;
                }
                s.arn.clone()
            }
        };
        match crate::clipboard::copy(&arn) {
            Ok(()) => self.status = format!("copied ARN: {arn}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    /// `P` — enter publish mode (topics only). Subsequent keystrokes
    /// build the message until Enter (publish) or Esc (cancel).
    pub fn enter_publish_mode(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Topic(_) = item else {
            self.status = "publish only available on topics".into();
            return;
        };
        self.publish_editing = Some(String::new());
        self.status = "publish: type message, Enter to send".into();
    }

    pub fn publish_input_char(&mut self, c: char) {
        if let Some(buf) = self.publish_editing.as_mut() {
            buf.push(c);
        }
    }

    pub fn publish_input_backspace(&mut self) {
        if let Some(buf) = self.publish_editing.as_mut() {
            buf.pop();
        }
    }

    /// Enter: call `aws sns publish --topic-arn X --message Y`.
    /// Empty messages are rejected with a status (SNS would reject
    /// them too — saves a round-trip).
    pub fn publish_commit(&mut self) {
        let Some(message) = self.publish_editing.take() else {
            return;
        };
        if message.is_empty() {
            self.status = "publish cancelled (empty message)".into();
            return;
        }
        let Some(item) = self.focused_item() else {
            self.status = "no topic under cursor".into();
            return;
        };
        let Item::Topic(t) = item else {
            self.status = "publish only available on topics".into();
            return;
        };
        let topic_arn = t.arn.clone();
        let topic_name = t.name().to_string();
        let region = self.active().spec.region.clone();

        let mut cmd = std::process::Command::new("aws");
        cmd.args([
            "sns",
            "publish",
            "--topic-arn",
            &topic_arn,
            "--message",
            &message,
            "--output",
            "json",
        ]);
        if let Some(r) = &region {
            cmd.args(["--region", r]);
        }
        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("publish failed (aws CLI: {e})");
                return;
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            self.status = format!("publish failed: {}", stderr.trim());
            return;
        }
        // Try to parse the MessageId out of the response; fall back to
        // a generic "published" status if the shape changed.
        let msg_id: Option<String> = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|v| {
                v.get("MessageId")
                    .and_then(|m| m.as_str())
                    .map(String::from)
            });
        self.status = match msg_id {
            Some(id) => format!("published to {topic_name} · MessageId {id}"),
            None => format!("published to {topic_name}"),
        };
    }

    pub fn publish_cancel(&mut self) {
        if self.publish_editing.take().is_some() {
            self.status = "publish cancelled".into();
        }
    }

    /// `L` — cross-sibling jump: when focused on a subscription with
    /// an SQS or Lambda endpoint, spawn the matching family sibling.
    /// Other endpoint protocols (HTTP, email, SMS, application) get a
    /// status message explaining there's no sibling to hand off to
    /// (yet — HTTP-endpoint introspection is a v0.x maybe).
    ///
    /// On topics: no-op with a status — the topic's *publish* surface
    /// would be a different action (`P` publish-test-message, v0.x).
    pub fn handoff_endpoint(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let Item::Subscription(s) = item else {
            self.status = "L jump is only available on subscriptions".into();
            return;
        };
        let Some(endpoint) = s.endpoint.as_deref() else {
            self.status = "subscription has no endpoint to jump to".into();
            return;
        };
        let protocol = s.protocol.as_deref().unwrap_or("");
        match protocol {
            "sqs" => {
                // Endpoint is an SQS queue ARN. mnml-aws-sqs doesn't
                // (yet) accept a single-ARN CLI flag — for v0.1 we
                // just spawn it bare and let the user navigate. v0.x
                // could thread a queue-URL flag through.
                let queue_name = endpoint.rsplit(':').next().unwrap_or(endpoint);
                match std::process::Command::new("mnml-aws-sqs").spawn() {
                    Ok(_) => {
                        self.status = format!(
                            "launched mnml-aws-sqs — navigate to {queue_name} (auto-scope is v0.x)"
                        );
                    }
                    Err(e) => {
                        self.status = format!("spawn mnml-aws-sqs failed (install it?): {e}");
                    }
                }
            }
            "lambda" => {
                let fn_name = endpoint.rsplit(':').next().unwrap_or(endpoint);
                match std::process::Command::new("mnml-aws-lambda").spawn() {
                    Ok(_) => {
                        self.status = format!(
                            "launched mnml-aws-lambda — navigate to {fn_name} (auto-scope is v0.x)"
                        );
                    }
                    Err(e) => {
                        self.status = format!("spawn mnml-aws-lambda failed (install it?): {e}");
                    }
                }
            }
            other => {
                self.status =
                    format!("no sibling for `{other}` endpoints — supported: sqs, lambda");
            }
        }
    }

    /// `Y` — yank focused subscription's endpoint (or topic's ARN if
    /// the focus is a topic). Useful when you want to drop a Lambda
    /// ARN, SQS queue ARN, or email straight into a config.
    pub fn yank_endpoint(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let payload = match item {
            Item::Topic(t) => t.arn.clone(),
            Item::Subscription(s) => s.endpoint.clone().unwrap_or_default(),
        };
        if payload.is_empty() {
            self.status = "no endpoint to copy".into();
            return;
        }
        match crate::clipboard::copy(&payload) {
            Ok(()) => self.status = format!("copied endpoint: {payload}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }
}

/// URL-encode a string for use as a path segment.
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
            kind: "topics".into(),
            topic_arn: None,
            prefix: None,
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_subscriptions_without_topic_arn() {
        let t = Tab {
            name: "bad".into(),
            kind: "subscriptions".into(),
            topic_arn: None,
            prefix: None,
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }

    #[test]
    fn item_subscription_secondary_label_flags_pending() {
        let s = Subscription {
            arn: "PendingConfirmation".into(),
            owner: None,
            protocol: Some("email".into()),
            endpoint: Some("ops@example.com".into()),
            topic_arn: None,
        };
        let label = Item::Subscription(s).secondary_label();
        assert!(label.contains("pending confirmation"));
    }
}
