//! Mandrill (Mailchimp Transactional) HTTP API client — blocking
//! `reqwest` + `serde_json`. No SDK dep.
//!
//! Every endpoint is `POST https://mandrillapp.com/api/1.0/<resource>/<verb>.json`
//! and the body always contains `{"key": "<MANDRILL_API_KEY>", ...}`.
//! There is no header auth — the API key lives in the body.
//!
//! Pagination is intentionally NOT exhaustive for v0.1: the API caps
//! `/messages/search.json` at ~1000 hits per request and we ask for
//! 100 by default. List endpoints (templates / tags / webhooks) return
//! everything in one shot, but we cap defensively at `LIST_CAP`.

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

/// Hard cap on items rendered per list tab.
pub const LIST_CAP: usize = 500;

/// `/messages/search.json` page size — Mandrill caps at 1000; 100 is
/// plenty for the recent-sends view.
pub const MESSAGES_LIMIT: usize = 100;

const API_BASE: &str = "https://mandrillapp.com/api/1.0";

/// Resolved auth — reads `MANDRILL_API_KEY` from the env. Missing key
/// is a hard error.
#[derive(Debug, Clone)]
pub struct Auth {
    pub api_key: String,
}

impl Auth {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("MANDRILL_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        match api_key {
            Some(api_key) => Ok(Self { api_key }),
            None => Err(anyhow!(
                "MANDRILL_API_KEY not set — export it from a Mandrill API key (Settings → SMTP & API Info → API Keys)"
            )),
        }
    }

    pub fn api_base(&self) -> &'static str {
        API_BASE
    }
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-msg-mandrill/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

/// Parse Mandrill's error envelope —
/// `{"status":"error","code":-1,"name":"ValidationError","message":"..."}`.
/// Falls back to a raw `HTTP {code}` string when the body doesn't fit
/// that shape.
fn extract_mandrill_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
        && v.get("status").and_then(|s| s.as_str()) == Some("error")
    {
        let name = v.get("name").and_then(|s| s.as_str()).unwrap_or("error");
        let msg = v
            .get("message")
            .and_then(|s| s.as_str())
            .unwrap_or("(no message)");
        return format!("mandrill: {name}: {msg}");
    }
    format!("mandrill: HTTP {}", status.as_u16())
}

/// POST helper — appends `"key"` to the supplied body, parses
/// Mandrill's error envelope on non-2xx.
fn post_json(auth: &Auth, path: &str, mut body: serde_json::Value) -> Result<String> {
    let client = build_client()?;
    let url = format!("{}{}", auth.api_base(), path);
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert(
            "key".to_string(),
            serde_json::Value::String(auth.api_key.clone()),
        );
    } else {
        return Err(anyhow!("internal: request body must be a JSON object"));
    }
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().with_context(|| format!("read body of {url}"))?;
    if !status.is_success() {
        return Err(anyhow!(extract_mandrill_error(status, &text)));
    }
    Ok(text)
}

/// `POST /users/ping.json` — auth + connectivity smoke test. Returns
/// the literal `"PONG!"` body on success.
pub fn ping(auth: &Auth) -> Result<String> {
    post_json(auth, "/users/ping.json", serde_json::json!({}))
}

// ── Messages (POST /messages/search.json) ────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    /// Mandrill message ID (e.g. `"abc123def456"`). Always present on
    /// search results.
    #[serde(rename = "_id", default)]
    pub id: String,
    #[serde(default)]
    pub subject: String,
    /// Recipient address.
    #[serde(default)]
    pub email: String,
    /// `sent` / `queued` / `scheduled` / `rejected` / `bounced` /
    /// `soft-bounced` / `spam` / `unsub` / `deferred` / `delivered`.
    #[serde(default)]
    pub state: String,
    /// Unix epoch seconds (Mandrill returns an integer).
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub opens: i64,
    #[serde(default)]
    pub clicks: i64,
}

impl Message {
    pub fn short_subject(&self) -> &str {
        if self.subject.is_empty() {
            "(no subject)"
        } else {
            self.subject.as_str()
        }
    }
}

/// `POST /messages/search.json`. Default `query` is `*` (everything);
/// date range is the last `lookback_days` days.
pub fn search_messages(auth: &Auth, lookback_days: u64) -> Result<Vec<Message>> {
    let today = chrono::Utc::now().date_naive();
    let from = today
        .checked_sub_signed(chrono::Duration::days(lookback_days as i64))
        .unwrap_or(today);
    let body = serde_json::json!({
        "query":     "*",
        "date_from": from.to_string(),
        "date_to":   today.to_string(),
        "limit":     MESSAGES_LIMIT,
    });
    let text = post_json(auth, "/messages/search.json", body)?;
    let mut messages: Vec<Message> =
        serde_json::from_str(&text).with_context(|| "parse messages JSON")?;
    // Mandrill returns most-recent-first already, but sort defensively.
    messages.sort_by_key(|m| std::cmp::Reverse(m.ts));
    if messages.len() > LIST_CAP {
        messages.truncate(LIST_CAP);
    }
    Ok(messages)
}

/// Full detail for a single message — `POST /messages/info.json`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `id` is set from the call-site arg; `metadata` / `subaccount` parsed for completeness.
pub struct MessageDetail {
    #[serde(rename = "_id", default)]
    pub id: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub opens: i64,
    #[serde(default)]
    pub clicks: i64,
    #[serde(default)]
    pub smtp_events: Vec<SmtpEvent>,
    #[serde(default)]
    pub opens_detail: Vec<OpenEvent>,
    #[serde(default)]
    pub clicks_detail: Vec<ClickEvent>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub subaccount: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SmtpEvent {
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub diag: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenEvent {
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub ua: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClickEvent {
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub ua: String,
}

pub fn message_info(auth: &Auth, id: &str) -> Result<MessageDetail> {
    let body = serde_json::json!({ "id": id });
    let text = post_json(auth, "/messages/info.json", body)?;
    serde_json::from_str(&text).with_context(|| "parse message-info JSON")
}

// ── Templates (POST /templates/list.json) ────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Some publish-side fields are parsed for future use (subject preview, from-name display, plain-text body).
pub struct Template {
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    /// `"sent"` / `"draft"` / `""`.
    #[serde(default)]
    pub publish_name: String,
    #[serde(default)]
    pub publish_code: Option<String>,
    #[serde(default)]
    pub publish_subject: Option<String>,
    #[serde(default)]
    pub publish_from_email: Option<String>,
    #[serde(default)]
    pub publish_from_name: Option<String>,
    #[serde(default)]
    pub publish_text: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub from_email: Option<String>,
    #[serde(default)]
    pub from_name: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Template {
    /// Best-effort "current subject" — published, else draft, else empty.
    pub fn display_subject(&self) -> &str {
        self.publish_subject
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.subject.as_deref())
            .unwrap_or("")
    }

    pub fn publish_state(&self) -> &str {
        // Mandrill marks a published template by setting `published_at`;
        // `publish_name` is the slug-when-published. Use `published_at`
        // to discriminate.
        if self
            .published_at
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some()
        {
            "published"
        } else {
            "draft"
        }
    }
}

pub fn list_templates(auth: &Auth) -> Result<Vec<Template>> {
    let text = post_json(auth, "/templates/list.json", serde_json::json!({}))?;
    let mut templates: Vec<Template> =
        serde_json::from_str(&text).with_context(|| "parse templates JSON")?;
    templates.sort_by_key(|t| t.name.to_lowercase());
    if templates.len() > LIST_CAP {
        templates.truncate(LIST_CAP);
    }
    Ok(templates)
}

/// Single-template info. Currently unused by the UI (we already have
/// the full template object from `list.json`), kept for v0.2 detail
/// re-fetches.
#[allow(dead_code)]
pub fn template_info(auth: &Auth, slug: &str) -> Result<Template> {
    let body = serde_json::json!({ "name": slug });
    let text = post_json(auth, "/templates/info.json", body)?;
    serde_json::from_str(&text).with_context(|| "parse template-info JSON")
}

// ── Tags (POST /tags/list.json) ──────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Tag {
    #[serde(default)]
    pub tag: String,
    /// First send time, ISO-ish string.
    #[serde(default)]
    pub reputation: i64,
    #[serde(default)]
    pub sent: i64,
    #[serde(default)]
    pub hard_bounces: i64,
    #[serde(default)]
    pub soft_bounces: i64,
    #[serde(default)]
    pub rejects: i64,
    #[serde(default)]
    pub complaints: i64,
    #[serde(default)]
    pub unsubs: i64,
    #[serde(default)]
    pub opens: i64,
    #[serde(default)]
    pub clicks: i64,
    #[serde(default)]
    pub unique_opens: i64,
    #[serde(default)]
    pub unique_clicks: i64,
}

impl Tag {
    /// Bounce rate as a 0..=1 fraction. Returns 0 when `sent == 0`.
    pub fn bounce_rate(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        let bounces = self.hard_bounces + self.soft_bounces;
        (bounces as f64) / (self.sent as f64)
    }
}

pub fn list_tags(auth: &Auth) -> Result<Vec<Tag>> {
    let text = post_json(auth, "/tags/list.json", serde_json::json!({}))?;
    let mut tags: Vec<Tag> = serde_json::from_str(&text).with_context(|| "parse tags JSON")?;
    tags.sort_by_key(|t| std::cmp::Reverse(t.sent));
    if tags.len() > LIST_CAP {
        tags.truncate(LIST_CAP);
    }
    Ok(tags)
}

/// `POST /tags/info.json` — adds a sparse time-series stat block to
/// what `list.json` returned. v0.1 surfaces the cumulative counters.
#[allow(dead_code)]
pub fn tag_info(auth: &Auth, tag: &str) -> Result<Tag> {
    let body = serde_json::json!({ "tag": tag });
    let text = post_json(auth, "/tags/info.json", body)?;
    serde_json::from_str(&text).with_context(|| "parse tag-info JSON")
}

// ── Webhooks (POST /webhooks/list.json) ──────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Webhook {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub auth_key: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_sent_at: Option<String>,
    #[serde(default)]
    pub batches_sent: i64,
    #[serde(default)]
    pub events_sent: i64,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Webhook {
    /// Last 4 chars of the webhook auth key, or `"—"` when empty.
    pub fn auth_key_trailing(&self) -> String {
        if self.auth_key.is_empty() {
            return "—".to_string();
        }
        if self.auth_key.len() <= 4 {
            return self.auth_key.clone();
        }
        format!("…{}", &self.auth_key[self.auth_key.len() - 4..])
    }
}

pub fn list_webhooks(auth: &Auth) -> Result<Vec<Webhook>> {
    let text = post_json(auth, "/webhooks/list.json", serde_json::json!({}))?;
    let mut hooks: Vec<Webhook> =
        serde_json::from_str(&text).with_context(|| "parse webhooks JSON")?;
    hooks.sort_by_key(|w| w.id);
    if hooks.len() > LIST_CAP {
        hooks.truncate(LIST_CAP);
    }
    Ok(hooks)
}

#[allow(dead_code)]
pub fn webhook_info(auth: &Auth, id: i64) -> Result<Webhook> {
    let body = serde_json::json!({ "id": id });
    let text = post_json(auth, "/webhooks/info.json", body)?;
    serde_json::from_str(&text).with_context(|| "parse webhook-info JSON")
}

// ── URL building helpers ─────────────────────────────────────────

pub fn message_url(id: &str) -> String {
    format!("https://mandrillapp.com/activity?details={id}")
}

pub fn template_url(slug: &str) -> String {
    format!(
        "https://mandrillapp.com/templates/code?id={}",
        urlencode(slug)
    )
}

pub fn tag_url(name: &str) -> String {
    format!("https://mandrillapp.com/tags/info?tag={}", urlencode(name))
}

pub fn webhooks_url() -> &'static str {
    "https://mandrillapp.com/settings/webhooks"
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
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

    // Env-touching tests share a process-wide mutex so they don't race
    // each other (`cargo test` parallelizes by default).
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn auth_reads_from_env_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("MANDRILL_API_KEY").ok();
        // SAFETY: serialized via ENV_LOCK; we restore below.
        unsafe { std::env::set_var("MANDRILL_API_KEY", "test-key-abc123") };
        let a = Auth::from_env().expect("loads from env");
        assert_eq!(a.api_key, "test-key-abc123");
        assert_eq!(a.api_base(), "https://mandrillapp.com/api/1.0");
        match prev {
            Some(v) => unsafe { std::env::set_var("MANDRILL_API_KEY", v) },
            None => unsafe { std::env::remove_var("MANDRILL_API_KEY") },
        }
    }

    #[test]
    fn auth_rejects_missing_or_empty_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("MANDRILL_API_KEY").ok();
        // Unset → error.
        unsafe { std::env::remove_var("MANDRILL_API_KEY") };
        let err = Auth::from_env().unwrap_err();
        assert!(err.to_string().contains("MANDRILL_API_KEY"));
        // Set to empty string → also error (the .filter(|s| !s.is_empty())).
        unsafe { std::env::set_var("MANDRILL_API_KEY", "") };
        assert!(Auth::from_env().is_err());
        if let Some(v) = prev {
            unsafe { std::env::set_var("MANDRILL_API_KEY", v) };
        } else {
            unsafe { std::env::remove_var("MANDRILL_API_KEY") };
        }
    }

    #[test]
    fn mandrill_error_envelope_extracted() {
        let body =
            r#"{"status":"error","code":-1,"name":"Invalid_Key","message":"Invalid API key"}"#;
        let msg = extract_mandrill_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(msg.starts_with("mandrill:"));
        assert!(msg.contains("Invalid_Key"));
        assert!(msg.contains("Invalid API key"));
    }

    #[test]
    fn http_error_without_envelope_falls_back() {
        let msg = extract_mandrill_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "<html>");
        assert_eq!(msg, "mandrill: HTTP 500");
    }

    #[test]
    fn parses_messages_search_json() {
        let json = r#"[
            {"_id":"abc","subject":"hi","email":"to@example.com","state":"sent","ts":1700000000,"sender":"from@example.com","tags":["welcome"],"opens":1,"clicks":0},
            {"_id":"def","subject":"bounced","email":"bad@example.com","state":"bounced","ts":1699999000,"sender":"from@example.com","tags":[],"opens":0,"clicks":0}
        ]"#;
        let msgs: Vec<Message> = serde_json::from_str(json).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, "abc");
        assert_eq!(msgs[1].state, "bounced");
    }

    #[test]
    fn parses_message_info_with_smtp_events() {
        let json = r#"{
            "_id":"abc","subject":"hi","email":"to@example.com","sender":"from@example.com",
            "state":"sent","ts":1700000000,"tags":["welcome"],"opens":1,"clicks":0,
            "smtp_events":[
                {"ts":1700000001,"type":"sent","diag":"250 OK"},
                {"ts":1700000002,"type":"delivered","diag":""}
            ],
            "opens_detail":[],
            "clicks_detail":[],
            "metadata":{},
            "subaccount":null
        }"#;
        let detail: MessageDetail = serde_json::from_str(json).unwrap();
        assert_eq!(detail.smtp_events.len(), 2);
        assert_eq!(detail.smtp_events[0].r#type, "sent");
    }

    #[test]
    fn parses_templates_list_json() {
        let json = r#"[
            {"slug":"welcome","name":"Welcome","publish_subject":"Welcome!","published_at":"2026-01-01 00:00:00","subject":"Welcome!","labels":[]},
            {"slug":"reset","name":"Password reset","publish_subject":null,"published_at":null,"subject":"Reset","labels":["auth"]}
        ]"#;
        let templates: Vec<Template> = serde_json::from_str(json).unwrap();
        assert_eq!(templates.len(), 2);
        assert_eq!(templates[0].publish_state(), "published");
        assert_eq!(templates[1].publish_state(), "draft");
        assert_eq!(templates[0].display_subject(), "Welcome!");
    }

    #[test]
    fn parses_tags_list_json() {
        let json = r#"[
            {"tag":"welcome","reputation":100,"sent":1000,"hard_bounces":5,"soft_bounces":3,"rejects":0,"complaints":0,"unsubs":1,"opens":600,"clicks":200,"unique_opens":500,"unique_clicks":180}
        ]"#;
        let tags: Vec<Tag> = serde_json::from_str(json).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag, "welcome");
        let br = tags[0].bounce_rate();
        assert!((br - 0.008).abs() < 1e-6, "bounce_rate = {br}");
    }

    #[test]
    fn tag_bounce_rate_zero_when_no_sends() {
        let t = Tag {
            tag: "x".into(),
            reputation: 0,
            sent: 0,
            hard_bounces: 0,
            soft_bounces: 0,
            rejects: 0,
            complaints: 0,
            unsubs: 0,
            opens: 0,
            clicks: 0,
            unique_opens: 0,
            unique_clicks: 0,
        };
        assert_eq!(t.bounce_rate(), 0.0);
    }

    #[test]
    fn parses_webhooks_list_json() {
        let json = r#"[
            {"id":42,"url":"https://hooks.example.com/mandrill","description":"prod hook","auth_key":"SECRET123XYZ9","events":["send","open","click"],"created_at":"2026-01-01 00:00:00","batches_sent":100,"events_sent":250}
        ]"#;
        let hooks: Vec<Webhook> = serde_json::from_str(json).unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].auth_key_trailing(), "…XYZ9");
        assert_eq!(hooks[0].events, vec!["send", "open", "click"]);
    }

    #[test]
    fn webhook_auth_key_trailing_handles_short_keys() {
        let w = Webhook {
            id: 1,
            url: "x".into(),
            description: None,
            auth_key: "abc".into(),
            events: vec![],
            created_at: None,
            last_sent_at: None,
            batches_sent: 0,
            events_sent: 0,
            last_error: None,
        };
        assert_eq!(w.auth_key_trailing(), "abc");
        let empty = Webhook {
            id: 2,
            url: "x".into(),
            description: None,
            auth_key: "".into(),
            events: vec![],
            created_at: None,
            last_sent_at: None,
            batches_sent: 0,
            events_sent: 0,
            last_error: None,
        };
        assert_eq!(empty.auth_key_trailing(), "—");
    }

    #[test]
    fn url_helpers_build_expected_urls() {
        assert_eq!(
            message_url("abc123"),
            "https://mandrillapp.com/activity?details=abc123"
        );
        assert_eq!(
            template_url("welcome"),
            "https://mandrillapp.com/templates/code?id=welcome"
        );
        assert_eq!(
            tag_url("user signup"),
            "https://mandrillapp.com/tags/info?tag=user%20signup"
        );
        assert_eq!(webhooks_url(), "https://mandrillapp.com/settings/webhooks");
    }
}
