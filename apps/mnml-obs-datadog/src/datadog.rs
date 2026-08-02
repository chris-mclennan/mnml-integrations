//! Datadog HTTP API client — blocking `reqwest` + `serde_json`. No
//! SDK dep. Hits v1 (monitors, dashboards) + v2 (logs search,
//! incidents) endpoints.
//!
//! Auth: `DD-API-KEY` + `DD-APPLICATION-KEY` headers, set per request.
//! Base URL: `https://api.{DD_SITE}/api/{v1,v2}/...`.
//!
//! Pagination is intentionally NOT exhaustive — for v0.1 we cap each
//! list and surface a `(N+ more)` hint in the secondary label when
//! the cap is hit. The Datadog list endpoints return everything by
//! default for monitors + dashboards, so the cap is a UI safety
//! valve; logs + incidents have proper cursor pagination which we
//! ignore for v0.1.

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_SITE: &str = "datadoghq.com";

/// Hard cap on items rendered per list tab. If the API returns more
/// than this, the UI shows a "(N+ more)" hint.
pub const LIST_CAP: usize = 500;

/// Logs search page size — Datadog's v2 logs API supports up to
/// 1000 per page; 100 is plenty for the live-tail view.
pub const LOGS_PAGE_LIMIT: usize = 100;

/// Resolved auth — reads `DD_API_KEY`, `DD_APP_KEY`, `DD_SITE` from
/// the env. Missing API_KEY or APP_KEY is a hard error.
#[derive(Debug, Clone)]
pub struct Auth {
    pub api_key: String,
    pub app_key: String,
    pub site: String,
}

impl Auth {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("DD_API_KEY").ok().filter(|s| !s.is_empty());
        let app_key = std::env::var("DD_APP_KEY").ok().filter(|s| !s.is_empty());
        let site = std::env::var("DD_SITE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_SITE.to_string());

        match (api_key, app_key) {
            (Some(api_key), Some(app_key)) => Ok(Self {
                api_key,
                app_key,
                site,
            }),
            (None, _) => Err(anyhow!(
                "DD_API_KEY not set — export it from a Datadog API key (Org Settings → API Keys)"
            )),
            (_, None) => Err(anyhow!(
                "DD_APP_KEY not set — export it from a Datadog application key (Org Settings → Application Keys)"
            )),
        }
    }

    pub fn api_base_v1(&self) -> String {
        format!("https://api.{}/api/v1", self.site)
    }
    pub fn api_base_v2(&self) -> String {
        format!("https://api.{}/api/v2", self.site)
    }

    /// `https://app.{site}` for building web-console URLs.
    pub fn app_base(&self) -> String {
        format!("https://app.{}", self.site)
    }
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-obs-datadog/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

/// Parse Datadog's `{"errors": ["..."]}` envelope when a non-2xx
/// response carries one. Falls back to the raw status line.
fn extract_dd_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(arr) = v.get("errors").and_then(|e| e.as_array())
        && let Some(first) = arr.first().and_then(|e| e.as_str())
    {
        return format!("datadog: {first}");
    }
    format!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

// ── Monitors (v1) ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Monitor {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub query: String,
    /// `metric alert`, `service check`, `log alert`, `event alert`,
    /// `synthetics alert`, etc.
    #[serde(rename = "type", default)]
    pub monitor_type: String,
    /// `OK` / `Alert` / `Warn` / `No Data` / `Skipped` / `Ignored` /
    /// `Unknown`.
    #[serde(default, rename = "overall_state")]
    pub overall_state: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// ISO-8601 timestamp; the API may serialize this several ways
    /// depending on the monitor — keep it as a raw string.
    #[serde(default)]
    pub modified: Option<String>,
}

impl Monitor {
    pub fn short_name(&self) -> &str {
        if self.name.is_empty() {
            "(unnamed)"
        } else {
            self.name.as_str()
        }
    }
}

/// `GET /api/v1/monitor`. Optionally scope by tag (comma-separated).
/// The endpoint returns the entire list (no cursor) — we cap at
/// `LIST_CAP` defensively.
pub fn list_monitors(auth: &Auth, tag_scope: Option<&str>) -> Result<Vec<Monitor>> {
    let client = build_client()?;
    let mut url = format!("{}/monitor", auth.api_base_v1());
    if let Some(tag) = tag_scope
        && !tag.is_empty()
    {
        url.push_str("?monitor_tags=");
        url.push_str(&urlencode(tag));
    }
    let resp = client
        .get(&url)
        .header("DD-API-KEY", &auth.api_key)
        .header("DD-APPLICATION-KEY", &auth.app_key)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read monitors body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_dd_error(status, &body)));
    }
    let mut monitors: Vec<Monitor> =
        serde_json::from_str(&body).with_context(|| "parse monitors JSON")?;
    monitors.sort_by(|a, b| {
        // Alerts first, then Warn, then No Data, then OK.
        rank_state(&a.overall_state)
            .cmp(&rank_state(&b.overall_state))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    if monitors.len() > LIST_CAP {
        monitors.truncate(LIST_CAP);
    }
    Ok(monitors)
}

fn rank_state(s: &str) -> u8 {
    match s {
        "Alert" => 0,
        "Warn" => 1,
        "No Data" => 2,
        "Skipped" | "Ignored" => 3,
        "OK" => 4,
        _ => 5,
    }
}

// ── Dashboards (v1) ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Dashboard {
    /// Short string id; build the URL at `{app_base}/dashboard/{id}`.
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author_handle: Option<String>,
    /// Build URL — Datadog returns this as a path on the dashboard
    /// list endpoint (e.g. `/dashboard/abc-def-ghi`).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub layout_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DashboardListResponse {
    #[serde(default)]
    dashboards: Vec<Dashboard>,
}

/// `GET /api/v1/dashboard`. Optionally filter by title-prefix
/// client-side.
pub fn list_dashboards(auth: &Auth, title_prefix: Option<&str>) -> Result<Vec<Dashboard>> {
    let client = build_client()?;
    let url = format!("{}/dashboard", auth.api_base_v1());
    let resp = client
        .get(&url)
        .header("DD-API-KEY", &auth.api_key)
        .header("DD-APPLICATION-KEY", &auth.app_key)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read dashboards body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_dd_error(status, &body)));
    }
    let parsed: DashboardListResponse =
        serde_json::from_str(&body).with_context(|| "parse dashboards JSON")?;
    let mut dashboards = parsed.dashboards;
    if let Some(p) = title_prefix
        && !p.is_empty()
    {
        let p_lc = p.to_lowercase();
        dashboards.retain(|d| d.title.to_lowercase().starts_with(&p_lc));
    }
    dashboards.sort_by_key(|d| d.title.to_lowercase());
    if dashboards.len() > LIST_CAP {
        dashboards.truncate(LIST_CAP);
    }
    Ok(dashboards)
}

// ── Logs (v2) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct LogEvent {
    pub id: String,
    #[serde(default)]
    pub attributes: LogEventAttributes,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogEventAttributes {
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    /// ISO-8601 string.
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LogsSearchResponse {
    #[serde(default)]
    data: Vec<LogEvent>,
}

/// `POST /api/v2/logs/events/search`. Body shape:
///
/// ```json
/// {
///   "filter": {"query": "<q>", "from": "now-15m", "to": "now"},
///   "page":   {"limit": 100},
///   "sort":   "-timestamp"
/// }
/// ```
pub fn search_logs(auth: &Auth, query: &str, from: &str) -> Result<Vec<LogEvent>> {
    let client = build_client()?;
    let url = format!("{}/logs/events/search", auth.api_base_v2());
    let body = serde_json::json!({
        "filter": { "query": query, "from": from, "to": "now" },
        "page":   { "limit": LOGS_PAGE_LIMIT },
        "sort":   "-timestamp"
    });
    let resp = client
        .post(&url)
        .header("DD-API-KEY", &auth.api_key)
        .header("DD-APPLICATION-KEY", &auth.app_key)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().with_context(|| "read logs body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_dd_error(status, &text)));
    }
    let parsed: LogsSearchResponse =
        serde_json::from_str(&text).with_context(|| "parse logs JSON")?;
    Ok(parsed.data)
}

// ── Incidents (v2) ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Incident {
    pub id: String,
    #[serde(default)]
    pub attributes: IncidentAttributes,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct IncidentAttributes {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub public_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct IncidentsResponse {
    #[serde(default)]
    data: Vec<Incident>,
}

/// `GET /api/v2/incidents?filter[state]=active` — open incidents
/// only. v0.1 doesn't deep-dive into an incident's timeline (that's
/// a v0.2 detail-pane).
pub fn list_active_incidents(auth: &Auth) -> Result<Vec<Incident>> {
    let client = build_client()?;
    let url = format!("{}/incidents?filter%5Bstate%5D=active", auth.api_base_v2());
    let resp = client
        .get(&url)
        .header("DD-API-KEY", &auth.api_key)
        .header("DD-APPLICATION-KEY", &auth.app_key)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let text = resp.text().with_context(|| "read incidents body")?;
    if !status.is_success() {
        return Err(anyhow!(extract_dd_error(status, &text)));
    }
    let parsed: IncidentsResponse =
        serde_json::from_str(&text).with_context(|| "parse incidents JSON")?;
    Ok(parsed.data)
}

// ── URL building helpers ─────────────────────────────────────────

pub fn monitor_url(auth: &Auth, id: i64) -> String {
    format!("{}/monitors/{id}", auth.app_base())
}

pub fn dashboard_url(auth: &Auth, d: &Dashboard) -> String {
    if let Some(path) = d.url.as_deref()
        && !path.is_empty()
    {
        // Datadog returns a leading `/dashboard/<id>` path.
        if path.starts_with("http") {
            return path.to_string();
        }
        return format!("{}{}", auth.app_base(), path);
    }
    format!("{}/dashboard/{}", auth.app_base(), d.id)
}

pub fn incident_url(auth: &Auth, inc: &Incident) -> String {
    let public_id = inc
        .attributes
        .public_id
        .map(|n| n.to_string())
        .unwrap_or_else(|| inc.id.clone());
    format!("{}/incidents/{public_id}", auth.app_base())
}

/// Logs explorer URL pre-scoped to a query.
pub fn logs_url(auth: &Auth, query: &str) -> String {
    format!("{}/logs?query={}", auth.app_base(), urlencode(query))
}

// ── Cross-sibling handoff helper ─────────────────────────────────

/// Walk a monitor query for an AWS log group reference. Returns
/// the group name if one is found, else None. The detection is
/// best-effort — Datadog monitors don't have a canonical log-group
/// field, so we scan the query string for common shapes.
pub fn extract_log_group(query: &str) -> Option<String> {
    // Most common — DD's `logs("...").index("...")` query embeds
    // the log group as a verbatim `aws_log_group:<name>` /
    // `log_group:<name>` tag. The tag can be anywhere in the query,
    // including inside a quoted string — `find` (not `split`) is the
    // robust scan.
    for prefix in ["aws_log_group:", "log_group:"] {
        if let Some(idx) = query.find(prefix) {
            let rest = &query[idx + prefix.len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == ')' || c == ',' || c == '\'')
                .unwrap_or(rest.len());
            let cleaned = &rest[..end];
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
    }
    // Raw `/aws/lambda/<fn-name>` token (mentioned in the message
    // or query body).
    for token in query.split(|c: char| c.is_whitespace() || c == '"' || c == ',' || c == ')') {
        if token.starts_with("/aws/") && token.len() > 5 {
            return Some(token.to_string());
        }
    }
    None
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
    fn auth_defaults_to_us1_site() {
        // Don't poke the user's real env — just smoke-test the
        // default-site fallback inside a fake instance.
        let a = Auth {
            api_key: "x".into(),
            app_key: "y".into(),
            site: DEFAULT_SITE.into(),
        };
        assert_eq!(a.api_base_v1(), "https://api.datadoghq.com/api/v1");
        assert_eq!(a.api_base_v2(), "https://api.datadoghq.com/api/v2");
        assert_eq!(a.app_base(), "https://app.datadoghq.com");
    }

    #[test]
    fn monitor_state_rank_alerts_first() {
        assert!(rank_state("Alert") < rank_state("Warn"));
        assert!(rank_state("Warn") < rank_state("No Data"));
        assert!(rank_state("No Data") < rank_state("OK"));
    }

    #[test]
    fn parses_monitors_json() {
        let json = r#"[
            {"id":1,"name":"high errors","overall_state":"Alert","type":"metric alert","query":"avg(...)","message":"msg","tags":["service:api"]},
            {"id":2,"name":"low traffic","overall_state":"OK","type":"metric alert","query":"avg(...)","message":"","tags":[]}
        ]"#;
        let mons: Vec<Monitor> = serde_json::from_str(json).unwrap();
        assert_eq!(mons.len(), 2);
        assert_eq!(mons[0].id, 1);
        assert_eq!(mons[0].overall_state, "Alert");
    }

    #[test]
    fn parses_dashboards_json() {
        let json = r#"{
            "dashboards":[
                {"id":"abc-def","title":"API overview","author_handle":"chris@example.com","url":"/dashboard/abc-def","layout_type":"ordered","modified_at":"2026-01-01T00:00:00Z"}
            ]
        }"#;
        let parsed: DashboardListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.dashboards.len(), 1);
        assert_eq!(parsed.dashboards[0].id, "abc-def");
    }

    #[test]
    fn parses_logs_search_json() {
        let json = r#"{
            "data":[
                {"id":"AAAA","attributes":{"service":"api","status":"error","message":"boom","host":"i-1","timestamp":"2026-01-01T00:00:00Z"}}
            ]
        }"#;
        let parsed: LogsSearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data.len(), 1);
        assert_eq!(parsed.data[0].attributes.service.as_deref(), Some("api"));
    }

    #[test]
    fn parses_incidents_json() {
        let json = r#"{
            "data":[
                {"id":"00000000-0000-0000-0000-000000000001","attributes":{"title":"DB outage","state":"active","severity":"SEV-2","public_id":42}}
            ]
        }"#;
        let parsed: IncidentsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data.len(), 1);
        assert_eq!(parsed.data[0].attributes.public_id, Some(42));
    }

    #[test]
    fn extracts_aws_log_group_from_monitor_query() {
        let q = r#"logs("aws_log_group:/aws/lambda/checkout-fn status:error").index("*").rollup("count").last("5m") > 10"#;
        assert_eq!(
            extract_log_group(q),
            Some("/aws/lambda/checkout-fn".to_string())
        );
    }

    #[test]
    fn extracts_raw_aws_lambda_path() {
        let q = "some text /aws/lambda/my-fn other";
        assert_eq!(extract_log_group(q), Some("/aws/lambda/my-fn".to_string()));
    }

    #[test]
    fn no_log_group_returns_none() {
        assert!(extract_log_group("metric:foo > 5").is_none());
    }

    #[test]
    fn dd_error_envelope_extracted() {
        let body = r#"{"errors":["API key invalid"]}"#;
        let msg = extract_dd_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(msg.contains("API key invalid"));
        assert!(msg.starts_with("datadog:"));
    }
}
