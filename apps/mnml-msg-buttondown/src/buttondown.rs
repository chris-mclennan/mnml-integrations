//! Buttondown HTTP API client — blocking `reqwest` + `serde_json`. No
//! SDK dep. Hits the v1 REST API at `https://api.buttondown.email/v1/`.
//!
//! Auth: `Authorization: Token <BUTTONDOWN_API_KEY>` header, set per
//! request.
//!
//! Pagination is intentionally NOT exhaustive — for v0.1 we fetch the
//! first page only and surface a `(N+ more)` hint when the response
//! reports more results than we received.
//!
//! Rate limits: Buttondown limits ~600 req/min. We don't auto-retry on
//! 429 — the error toast surfaces the status and the user can press
//! `r` again.

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://api.buttondown.email/v1";
const WEB_BASE: &str = "https://buttondown.email";

/// Page size — Buttondown defaults to 100; we keep that for v0.1.
/// Surfaced for the v0.2 paginator to consume.
#[allow(dead_code)]
pub const PAGE_SIZE: usize = 100;

/// Resolved auth — reads `BUTTONDOWN_API_KEY` from the env. Missing
/// key is a hard error.
#[derive(Debug, Clone)]
pub struct Auth {
    pub api_key: String,
}

impl Auth {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("BUTTONDOWN_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        match api_key {
            Some(api_key) => Ok(Self { api_key }),
            None => Err(anyhow!(
                "BUTTONDOWN_API_KEY not set — export it from Buttondown (Settings → Programming)"
            )),
        }
    }

    pub fn api_base(&self) -> &'static str {
        API_BASE
    }

    pub fn web_base(&self) -> &'static str {
        WEB_BASE
    }
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-msg-buttondown/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

/// Parse Buttondown's error envelopes:
///   - 4xx detail: `{"detail": "..."}`
///   - validation errors: `{"non_field_errors": ["..."]}` or other
///     per-field arrays
///
/// Falls back to the raw status line.
pub fn extract_bd_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(detail) = v.get("detail").and_then(|d| d.as_str()) {
            return format!("buttondown: {detail}");
        }
        if let Some(arr) = v.get("non_field_errors").and_then(|e| e.as_array())
            && let Some(first) = arr.first().and_then(|e| e.as_str())
        {
            return format!("buttondown: {first}");
        }
        // Other shapes: try to surface the first string in the first
        // array field (per-field validation errors).
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let Some(arr) = val.as_array()
                    && let Some(first) = arr.first().and_then(|e| e.as_str())
                {
                    return format!("buttondown: {k}: {first}");
                }
            }
        }
    }
    format!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

// ── Emails (drafts / sent / scheduled) ───────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Email {
    pub id: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub status: Option<String>,
    /// `public` / `private` / `premium` etc. — Buttondown calls this
    /// the "email type".
    #[serde(default)]
    pub email_type: Option<String>,
    #[serde(default)]
    pub creation_date: Option<String>,
    #[serde(default)]
    pub publish_date: Option<String>,
    #[serde(default)]
    pub modification_date: Option<String>,
    /// Buttondown reports word count + analytics on sent emails.
    #[serde(default)]
    pub word_count: Option<u32>,
    /// Sent-only — open / click stats. Reported as raw counts.
    #[serde(default)]
    pub analytics: Option<EmailAnalytics>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailAnalytics {
    #[serde(default)]
    pub recipients: Option<u32>,
    #[serde(default)]
    pub opens: Option<u32>,
    #[serde(default)]
    pub clicks: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct EmailsPage {
    #[serde(default)]
    results: Vec<Email>,
    #[serde(default)]
    count: Option<u32>,
}

/// `GET /emails?status=<status>` — first page only. Returns `(items,
/// total_count)`. The caller computes whether the list is truncated.
pub fn list_emails(auth: &Auth, status: &str) -> Result<(Vec<Email>, Option<u32>)> {
    let client = build_client()?;
    let url = format!(
        "{}/emails?status={}&page=1",
        auth.api_base(),
        urlencode(status)
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("Token {}", auth.api_key))
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let http_status = resp.status();
    let body = resp.text().with_context(|| "read emails body")?;
    if !http_status.is_success() {
        return Err(anyhow!(extract_bd_error(http_status, &body)));
    }
    let parsed: EmailsPage = serde_json::from_str(&body).with_context(|| "parse emails JSON")?;
    Ok((parsed.results, parsed.count))
}

/// `PATCH /emails/{id}` — schedule a draft for publishing. v0.1 ships
/// a fixed offset (5 minutes from now); v0.2 will add a date picker.
pub fn schedule_draft(auth: &Auth, id: &str, publish_date_iso: &str) -> Result<()> {
    let client = build_client()?;
    let url = format!("{}/emails/{}", auth.api_base(), id);
    let body = serde_json::json!({
        "status": "scheduled",
        "publish_date": publish_date_iso,
    });
    let resp = client
        .patch(&url)
        .header("Authorization", format!("Token {}", auth.api_key))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("PATCH {url}"))?;
    let http_status = resp.status();
    let text = resp.text().with_context(|| "read schedule body")?;
    if !http_status.is_success() {
        return Err(anyhow!(extract_bd_error(http_status, &text)));
    }
    Ok(())
}

// ── Subscribers ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Subscriber {
    pub id: String,
    #[serde(default)]
    pub email_address: String,
    /// `regular` / `premium` / `unactivated` / `unpaid` /
    /// `unsubscribed` / `removed` etc.
    #[serde(default, rename = "type")]
    pub sub_type: Option<String>,
    #[serde(default)]
    pub creation_date: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Optional. Where this subscriber came from — `import`, `api`,
    /// `embed`, `widget`, etc.
    #[serde(default)]
    pub source: Option<String>,
    /// Free-form metadata bag.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SubscribersPage {
    #[serde(default)]
    results: Vec<Subscriber>,
    #[serde(default)]
    count: Option<u32>,
}

/// `GET /subscribers?page=1` — first page only. Returns `(items,
/// total_count)`.
pub fn list_subscribers(auth: &Auth) -> Result<(Vec<Subscriber>, Option<u32>)> {
    let client = build_client()?;
    let url = format!("{}/subscribers?page=1", auth.api_base());
    let resp = client
        .get(&url)
        .header("Authorization", format!("Token {}", auth.api_key))
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let http_status = resp.status();
    let body = resp.text().with_context(|| "read subscribers body")?;
    if !http_status.is_success() {
        return Err(anyhow!(extract_bd_error(http_status, &body)));
    }
    let parsed: SubscribersPage =
        serde_json::from_str(&body).with_context(|| "parse subscribers JSON")?;
    Ok((parsed.results, parsed.count))
}

/// `DELETE /subscribers/{id}` — remove a subscriber.
pub fn unsubscribe(auth: &Auth, id: &str) -> Result<()> {
    let client = build_client()?;
    let url = format!("{}/subscribers/{}", auth.api_base(), id);
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Token {}", auth.api_key))
        .send()
        .with_context(|| format!("DELETE {url}"))?;
    let http_status = resp.status();
    let text = resp.text().with_context(|| "read unsubscribe body")?;
    if !http_status.is_success() {
        return Err(anyhow!(extract_bd_error(http_status, &text)));
    }
    Ok(())
}

// ── URL building helpers ─────────────────────────────────────────

pub fn email_url(auth: &Auth, id: &str) -> String {
    format!("{}/emails/{id}", auth.web_base())
}

pub fn subscriber_url(auth: &Auth, id: &str) -> String {
    format!("{}/subscribers/{id}", auth.web_base())
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

    #[test]
    fn auth_base_urls() {
        let a = Auth {
            api_key: "k".into(),
        };
        assert_eq!(a.api_base(), "https://api.buttondown.email/v1");
        assert_eq!(a.web_base(), "https://buttondown.email");
    }

    #[test]
    fn missing_api_key_errors() {
        // Snapshot the env var so we don't clobber a real key.
        let saved = std::env::var("BUTTONDOWN_API_KEY").ok();
        // SAFETY: tests are single-threaded around env access here;
        // we restore the original value before returning.
        unsafe {
            std::env::remove_var("BUTTONDOWN_API_KEY");
        }
        let r = Auth::from_env();
        assert!(r.is_err());
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("BUTTONDOWN_API_KEY", v);
            }
        }
    }

    #[test]
    fn parses_drafts_emails_json() {
        let json = r##"{
            "count": 3,
            "results": [
                {"id":"abc-1","subject":"Hello world","body":"# heading\n\nbody","status":"draft","creation_date":"2026-05-01T00:00:00Z","word_count":42}
            ]
        }"##;
        let parsed: EmailsPage = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.count, Some(3));
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].id, "abc-1");
        assert_eq!(parsed.results[0].subject, "Hello world");
        assert_eq!(parsed.results[0].word_count, Some(42));
    }

    #[test]
    fn parses_sent_emails_with_analytics() {
        let json = r#"{
            "count": 1,
            "results": [
                {"id":"sent-1","subject":"Issue 5","body":"body","status":"sent","email_type":"public","publish_date":"2026-04-01T00:00:00Z","analytics":{"recipients":1200,"opens":650,"clicks":42}}
            ]
        }"#;
        let parsed: EmailsPage = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.results.len(), 1);
        let a = parsed.results[0].analytics.as_ref().unwrap();
        assert_eq!(a.recipients, Some(1200));
        assert_eq!(a.opens, Some(650));
        assert_eq!(a.clicks, Some(42));
    }

    #[test]
    fn parses_scheduled_emails_json() {
        let json = r#"{
            "count": 1,
            "results": [
                {"id":"sch-1","subject":"Tomorrow","body":"body","status":"scheduled","publish_date":"2026-06-08T13:00:00Z"}
            ]
        }"#;
        let parsed: EmailsPage = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].status.as_deref(), Some("scheduled"));
        assert_eq!(
            parsed.results[0].publish_date.as_deref(),
            Some("2026-06-08T13:00:00Z")
        );
    }

    #[test]
    fn parses_subscribers_json() {
        let json = r#"{
            "count": 250,
            "results": [
                {"id":"sub-1","email_address":"a@example.com","type":"regular","creation_date":"2026-01-01T00:00:00Z","notes":"VIP","source":"embed"},
                {"id":"sub-2","email_address":"b@example.com","type":"premium","creation_date":"2026-02-02T00:00:00Z"}
            ]
        }"#;
        let parsed: SubscribersPage = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.count, Some(250));
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[1].sub_type.as_deref(), Some("premium"));
    }

    #[test]
    fn bd_error_detail_envelope() {
        let body = r#"{"detail":"Invalid token."}"#;
        let msg = extract_bd_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(msg.contains("Invalid token"));
        assert!(msg.starts_with("buttondown:"));
    }

    #[test]
    fn bd_error_non_field_errors_envelope() {
        let body = r#"{"non_field_errors":["Cannot schedule an already-sent email."]}"#;
        let msg = extract_bd_error(reqwest::StatusCode::BAD_REQUEST, body);
        assert!(msg.contains("Cannot schedule"));
        assert!(msg.starts_with("buttondown:"));
    }

    #[test]
    fn bd_error_per_field_validation_envelope() {
        let body = r#"{"publish_date":["This field is required."]}"#;
        let msg = extract_bd_error(reqwest::StatusCode::BAD_REQUEST, body);
        assert!(msg.contains("publish_date"));
        assert!(msg.contains("required"));
    }

    #[test]
    fn email_and_subscriber_url_shapes() {
        let a = Auth {
            api_key: "k".into(),
        };
        assert_eq!(
            email_url(&a, "abc-1"),
            "https://buttondown.email/emails/abc-1"
        );
        assert_eq!(
            subscriber_url(&a, "sub-1"),
            "https://buttondown.email/subscribers/sub-1"
        );
    }
}
