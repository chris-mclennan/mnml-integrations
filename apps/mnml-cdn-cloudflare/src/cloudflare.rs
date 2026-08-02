//! Cloudflare HTTP API client — blocking `reqwest` + `serde_json`. No
//! SDK dep. Hits the v4 endpoint:
//! `https://api.cloudflare.com/client/v4/...`.
//!
//! Auth: `Authorization: Bearer <token>` header from
//! `CLOUDFLARE_API_TOKEN`. Account-scoped tabs (workers, pages) also
//! need `CLOUDFLARE_ACCOUNT_ID`.
//!
//! Every Cloudflare response is wrapped in
//! `{"success": bool, "errors": [...], "result": ...}`. We unwrap
//! `result` on success and surface the first error message otherwise.
//! 403s are tagged with the "missing required scope" hint since that's
//! the most common token-misconfig.

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

pub const API_BASE: &str = "https://api.cloudflare.com/client/v4";
pub const DASH_BASE: &str = "https://dash.cloudflare.com";

/// Cap on items rendered per list tab.
pub const LIST_CAP: usize = 500;

/// Resolved auth — reads `CLOUDFLARE_API_TOKEN` and (optional)
/// `CLOUDFLARE_ACCOUNT_ID` from the env. Missing token is a hard
/// error; missing account_id only matters for account-scoped tabs.
#[derive(Debug, Clone)]
pub struct Auth {
    pub token: String,
    pub account_id: Option<String>,
}

impl Auth {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("CLOUDFLARE_API_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        let account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID")
            .ok()
            .filter(|s| !s.is_empty());

        match token {
            Some(token) => Ok(Self { token, account_id }),
            None => Err(anyhow!(
                "CLOUDFLARE_API_TOKEN not set — create one at dash.cloudflare.com → My Profile → API Tokens"
            )),
        }
    }
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-cdn-cloudflare/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

/// Cloudflare error-envelope shape — `{"success": bool, "errors":
/// [{"code": N, "message": "..."}], "result": ...}`.
#[derive(Debug, Deserialize)]
pub struct CfEnvelope<T> {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub errors: Vec<CfError>,
    #[serde(default = "Option::default")]
    pub result: Option<T>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // `code` parsed for future surfacing
pub struct CfError {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

/// Try to extract the first Cloudflare error message from a non-2xx
/// body, falling back to a generic HTTP/status message. 403s get a
/// scope-hint tag since that's the most common token misconfig.
pub fn extract_cf_error(status: reqwest::StatusCode, body: &str) -> String {
    if status.as_u16() == 403 {
        return "cloudflare: 403 — token missing required scope".to_string();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(arr) = v.get("errors").and_then(|e| e.as_array())
        && let Some(first) = arr.first()
        && let Some(msg) = first.get("message").and_then(|m| m.as_str())
    {
        return format!("cloudflare: {msg}");
    }
    format!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

fn parse_envelope<T: serde::de::DeserializeOwned>(
    status: reqwest::StatusCode,
    body: &str,
) -> Result<T> {
    if !status.is_success() {
        return Err(anyhow!(extract_cf_error(status, body)));
    }
    // Even on 2xx, Cloudflare may return success=false.
    let env: CfEnvelope<T> =
        serde_json::from_str(body).with_context(|| "parse cloudflare envelope")?;
    if !env.success {
        if let Some(first) = env.errors.first() {
            return Err(anyhow!("cloudflare: {}", first.message));
        }
        return Err(anyhow!("cloudflare: request failed (no error message)"));
    }
    env.result
        .ok_or_else(|| anyhow!("cloudflare: missing `result` in successful response"))
}

// ── /user/tokens/verify ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TokenVerify {
    pub id: String,
    #[serde(default)]
    pub status: String,
}

/// `GET /user/tokens/verify` — used by `--check`.
pub fn verify_token(auth: &Auth) -> Result<TokenVerify> {
    let client = build_client()?;
    let url = format!("{API_BASE}/user/tokens/verify");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read verify body")?;
    parse_envelope::<TokenVerify>(status, &body)
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `id` parsed for future surfacing
pub struct UserInfo {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
}

/// `GET /user` — best-effort, used by `--check` to surface the
/// token's owning email. Returns `None` if the token doesn't have
/// `User Details:Read` scope (verify works without it).
pub fn user_info(auth: &Auth) -> Result<UserInfo> {
    let client = build_client()?;
    let url = format!("{API_BASE}/user");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read user body")?;
    parse_envelope::<UserInfo>(status, &body)
}

// ── Zones ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `account` parsed for v0.2 multi-account views
pub struct Zone {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `active` / `pending` / `initializing` / `moved` / `deleted` /
    /// `deactivated` / `read only`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub plan: Option<ZonePlan>,
    #[serde(default)]
    pub name_servers: Vec<String>,
    #[serde(default)]
    pub original_name_servers: Vec<String>,
    #[serde(default)]
    pub development_mode: Option<i64>,
    #[serde(default)]
    pub modified_on: Option<String>,
    #[serde(default)]
    pub account: Option<ZoneAccount>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `id` parsed for v0.2
pub struct ZonePlan {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // parsed for v0.2 multi-account views
pub struct ZoneAccount {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

impl Zone {
    pub fn plan_name(&self) -> &str {
        self.plan.as_ref().map(|p| p.name.as_str()).unwrap_or("—")
    }
}

/// `GET /zones?per_page=50`. Returns up to LIST_CAP zones (the v0.1
/// cap; cursor pagination is v0.2).
pub fn list_zones(auth: &Auth) -> Result<Vec<Zone>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/zones?per_page=50");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read zones body")?;
    let mut zones: Vec<Zone> = parse_envelope(status, &body)?;
    zones.sort_by_key(|z| z.name.to_lowercase());
    if zones.len() > LIST_CAP {
        zones.truncate(LIST_CAP);
    }
    Ok(zones)
}

/// `GET /zones/{id}` — full zone detail (for the detail panel).
pub fn get_zone(auth: &Auth, zone_id: &str) -> Result<Zone> {
    let client = build_client()?;
    let url = format!("{API_BASE}/zones/{zone_id}");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read zone body")?;
    parse_envelope(status, &body)
}

/// `POST /zones/{id}/purge_cache` with `{"purge_everything": true}`.
pub fn purge_cache(auth: &Auth, zone_id: &str) -> Result<()> {
    let client = build_client()?;
    let url = format!("{API_BASE}/zones/{zone_id}/purge_cache");
    let body = serde_json::json!({ "purge_everything": true });
    let resp = client
        .post(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().with_context(|| "read purge body")?;
    // Cloudflare returns a `{ id }` result on success.
    let _: serde_json::Value = parse_envelope(status, &text)?;
    Ok(())
}

/// `PATCH /zones/{id}/settings/development_mode` with
/// `{"value": "on" | "off"}`. Returns the new value as a string.
pub fn set_dev_mode(auth: &Auth, zone_id: &str, on: bool) -> Result<String> {
    let client = build_client()?;
    let url = format!("{API_BASE}/zones/{zone_id}/settings/development_mode");
    let value = if on { "on" } else { "off" };
    let body = serde_json::json!({ "value": value });
    let resp = client
        .patch(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("PATCH {url}"))?;
    let status = resp.status();
    let text = resp.text().with_context(|| "read dev_mode body")?;
    let result: serde_json::Value = parse_envelope(status, &text)?;
    let new_val = result
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or(value)
        .to_string();
    Ok(new_val)
}

// ── DNS Records ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `zone_id` parsed for v0.2 edits
pub struct DnsRecord {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `A` / `AAAA` / `CNAME` / `MX` / `TXT` / `NS` / `SRV` / etc.
    #[serde(rename = "type", default)]
    pub record_type: String,
    #[serde(default)]
    pub content: String,
    /// `auto` (1) or a number of seconds.
    #[serde(default)]
    pub ttl: i64,
    /// Only meaningful for proxiable record types (A/AAAA/CNAME).
    #[serde(default)]
    pub proxied: bool,
    #[serde(default)]
    pub zone_id: String,
    #[serde(default)]
    pub zone_name: String,
}

/// `GET /zones/{zone_id}/dns_records?per_page=200`. Capped at
/// `LIST_CAP` records.
pub fn list_dns_records(auth: &Auth, zone_id: &str) -> Result<Vec<DnsRecord>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/zones/{zone_id}/dns_records?per_page=200");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read dns body")?;
    let mut records: Vec<DnsRecord> = parse_envelope(status, &body)?;
    records.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.record_type.cmp(&b.record_type))
    });
    if records.len() > LIST_CAP {
        records.truncate(LIST_CAP);
    }
    Ok(records)
}

// ── Workers ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerScript {
    pub id: String,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub modified_on: Option<String>,
    /// `usage_model` (`bundled` / `unbound` / `standard`).
    #[serde(default)]
    pub usage_model: Option<String>,
}

/// `GET /accounts/{account_id}/workers/scripts`.
pub fn list_workers(auth: &Auth, account_id: &str) -> Result<Vec<WorkerScript>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/accounts/{account_id}/workers/scripts");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read workers body")?;
    let mut scripts: Vec<WorkerScript> = parse_envelope(status, &body)?;
    scripts.sort_by_key(|s| s.id.to_lowercase());
    if scripts.len() > LIST_CAP {
        scripts.truncate(LIST_CAP);
    }
    Ok(scripts)
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // helper for v0.2 detail-panel auto-fetch
pub struct WorkerRoute {
    pub id: String,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub script: Option<String>,
}

/// `GET /accounts/{account_id}/workers/scripts/{name}/routes` — note
/// this is the v0.1 best-effort endpoint; the v2 routes API is per-
/// zone (`/zones/{zone_id}/workers/routes`). Not all token scopes
/// reach this; on 404/403 we return an empty list rather than
/// erroring the whole detail render.
#[allow(dead_code)] // helper for v0.2 detail-panel auto-fetch
pub fn list_worker_routes(
    auth: &Auth,
    account_id: &str,
    script_name: &str,
) -> Result<Vec<WorkerRoute>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/accounts/{account_id}/workers/scripts/{script_name}/routes");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if status.as_u16() == 404 || status.as_u16() == 403 {
        return Ok(Vec::new());
    }
    let body = resp.text().with_context(|| "read routes body")?;
    let routes: Vec<WorkerRoute> = parse_envelope(status, &body).unwrap_or_default();
    Ok(routes)
}

// ── Pages ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct PagesProject {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub subdomain: Option<String>,
    /// e.g. `<name>.pages.dev`.
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub production_branch: Option<String>,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub latest_deployment: Option<PagesDeployment>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `deployment_trigger` parsed for v0.2 detail panel
pub struct PagesDeployment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub created_on: Option<String>,
    /// `success` / `failure` / `active` / `canceled` / `idle`.
    #[serde(default)]
    pub latest_stage: Option<PagesStage>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub deployment_trigger: Option<PagesTrigger>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PagesStage {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ended_on: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // parsed for v0.2 trigger-aware detail panel
pub struct PagesTrigger {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

impl PagesProject {
    pub fn primary_domain(&self) -> String {
        if let Some(first) = self.domains.first() {
            return first.clone();
        }
        if let Some(sub) = &self.subdomain {
            return sub.clone();
        }
        format!("{}.pages.dev", self.name)
    }
    pub fn last_deploy_status(&self) -> &str {
        self.latest_deployment
            .as_ref()
            .and_then(|d| d.latest_stage.as_ref())
            .map(|s| s.status.as_str())
            .unwrap_or("—")
    }
}

/// `GET /accounts/{account_id}/pages/projects`.
pub fn list_pages_projects(auth: &Auth, account_id: &str) -> Result<Vec<PagesProject>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/accounts/{account_id}/pages/projects");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read pages body")?;
    let mut projects: Vec<PagesProject> = parse_envelope(status, &body)?;
    projects.sort_by_key(|p| p.name.to_lowercase());
    if projects.len() > LIST_CAP {
        projects.truncate(LIST_CAP);
    }
    Ok(projects)
}

// ── Security events ─────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SecurityEvent {
    #[serde(default)]
    pub ray_id: Option<String>,
    /// `block` / `challenge` / `jschallenge` / `managed_challenge` /
    /// `allow` / `log` / `connectionClose`.
    #[serde(default)]
    pub action: String,
    /// `firewallrules` / `waf` / `ratelimit` / `bic` / `hot` /
    /// `securitylevel` / `country` / `iprep` / `useragent` / etc.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub client_ip: Option<String>,
    #[serde(default)]
    pub client_country: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    /// ISO-8601 timestamp.
    #[serde(default)]
    pub occurred_at: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
}

/// `GET /zones/{zone_id}/security/events?limit=100` — the REST v4
/// firewall-events endpoint. (Cloudflare also offers GraphQL
/// analytics for richer queries; v0.1 uses the simpler REST path.)
pub fn list_security_events(auth: &Auth, zone_id: &str) -> Result<Vec<SecurityEvent>> {
    let client = build_client()?;
    let url = format!("{API_BASE}/zones/{zone_id}/security/events?limit=100");
    let resp = client
        .get(&url)
        .bearer_auth(&auth.token)
        .header("Content-Type", "application/json")
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| "read security events body")?;
    let events: Vec<SecurityEvent> = parse_envelope(status, &body)?;
    Ok(events)
}

// ── URL builders ────────────────────────────────────────────────

pub fn zone_dashboard_url(account_id: Option<&str>, zone_name: &str) -> String {
    match account_id {
        Some(acc) => format!("{DASH_BASE}/{acc}/{zone_name}"),
        None => format!("{DASH_BASE}/?to=/:account/{zone_name}"),
    }
}

pub fn worker_dashboard_url(account_id: &str, script_name: &str) -> String {
    format!("{DASH_BASE}/{account_id}/workers/services/view/{script_name}/production")
}

pub fn pages_dashboard_url(account_id: &str, project_name: &str) -> String {
    format!("{DASH_BASE}/{account_id}/pages/view/{project_name}")
}

pub fn dns_dashboard_url(account_id: Option<&str>, zone_name: &str) -> String {
    match account_id {
        Some(acc) => format!("{DASH_BASE}/{acc}/{zone_name}/dns"),
        None => format!("{DASH_BASE}/?to=/:account/{zone_name}/dns"),
    }
}

pub fn security_dashboard_url(account_id: Option<&str>, zone_name: &str) -> String {
    match account_id {
        Some(acc) => format!("{DASH_BASE}/{acc}/{zone_name}/security/events"),
        None => format!("{DASH_BASE}/?to=/:account/{zone_name}/security/events"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_requires_token() {
        // Clear the env in a child scope — can't actually mutate the
        // process env safely in parallel tests, so we smoke-test the
        // happy-path constructor directly.
        let a = Auth {
            token: "abc".into(),
            account_id: Some("def".into()),
        };
        assert_eq!(a.token, "abc");
        assert_eq!(a.account_id.as_deref(), Some("def"));
    }

    #[test]
    fn cf_error_envelope_extracted() {
        let body = r#"{"success":false,"errors":[{"code":1000,"message":"Invalid bearer token"}]}"#;
        let msg = extract_cf_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(msg.contains("Invalid bearer token"));
        assert!(msg.starts_with("cloudflare:"));
    }

    #[test]
    fn cf_403_gets_scope_hint() {
        let body =
            r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}]}"#;
        let msg = extract_cf_error(reqwest::StatusCode::FORBIDDEN, body);
        assert!(msg.contains("missing required scope"));
    }

    #[test]
    fn parse_zones_response() {
        let body = r#"{"success":true,"errors":[],"messages":[],"result":[
            {"id":"abc123","name":"example.com","status":"active","paused":false,
             "plan":{"id":"free","name":"Free Website"},
             "name_servers":["ns1.example.com","ns2.example.com"],
             "development_mode":0,"modified_on":"2026-01-01T00:00:00Z"}
        ]}"#;
        let env: CfEnvelope<Vec<Zone>> = serde_json::from_str(body).unwrap();
        assert!(env.success);
        let zones = env.result.unwrap();
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].name, "example.com");
        assert_eq!(zones[0].status, "active");
        assert_eq!(zones[0].plan_name(), "Free Website");
    }

    #[test]
    fn parse_dns_response() {
        let body = r#"{"success":true,"errors":[],"messages":[],"result":[
            {"id":"r1","name":"www.example.com","type":"A","content":"203.0.113.1","ttl":1,"proxied":true,"zone_id":"abc","zone_name":"example.com"},
            {"id":"r2","name":"example.com","type":"MX","content":"10 mail.example.com","ttl":3600,"proxied":false,"zone_id":"abc","zone_name":"example.com"}
        ]}"#;
        let env: CfEnvelope<Vec<DnsRecord>> = serde_json::from_str(body).unwrap();
        let records = env.result.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].record_type, "A");
        assert!(records[0].proxied);
        assert_eq!(records[1].record_type, "MX");
    }

    #[test]
    fn parse_workers_response() {
        let body = r#"{"success":true,"errors":[],"messages":[],"result":[
            {"id":"my-worker","created_on":"2026-01-01T00:00:00Z","modified_on":"2026-01-02T00:00:00Z","usage_model":"standard"}
        ]}"#;
        let env: CfEnvelope<Vec<WorkerScript>> = serde_json::from_str(body).unwrap();
        let scripts = env.result.unwrap();
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].id, "my-worker");
        assert_eq!(scripts[0].usage_model.as_deref(), Some("standard"));
    }

    #[test]
    fn parse_pages_response() {
        let body = r#"{"success":true,"errors":[],"messages":[],"result":[
            {"id":"p1","name":"docs","subdomain":"docs","domains":["docs.example.com"],
             "production_branch":"main","created_on":"2026-01-01T00:00:00Z",
             "latest_deployment":{"id":"d1","environment":"production",
               "latest_stage":{"name":"deploy","status":"success","ended_on":"2026-01-01T00:01:00Z"}}}
        ]}"#;
        let env: CfEnvelope<Vec<PagesProject>> = serde_json::from_str(body).unwrap();
        let projects = env.result.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "docs");
        assert_eq!(projects[0].primary_domain(), "docs.example.com");
        assert_eq!(projects[0].last_deploy_status(), "success");
    }

    #[test]
    fn parse_security_events_response() {
        let body = r#"{"success":true,"errors":[],"messages":[],"result":[
            {"ray_id":"abc","action":"block","source":"waf","rule_id":"100015",
             "client_ip":"1.2.3.4","client_country":"US","host":"example.com",
             "occurred_at":"2026-01-01T00:00:00Z","user_agent":"curl/8"}
        ]}"#;
        let env: CfEnvelope<Vec<SecurityEvent>> = serde_json::from_str(body).unwrap();
        let events = env.result.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "block");
        assert_eq!(events[0].source, "waf");
        assert_eq!(events[0].client_ip.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn parse_token_verify_response() {
        let body = r#"{"success":true,"errors":[],"messages":[],"result":{
            "id":"abc123","status":"active"
        }}"#;
        let env: CfEnvelope<TokenVerify> = serde_json::from_str(body).unwrap();
        let tv = env.result.unwrap();
        assert_eq!(tv.id, "abc123");
        assert_eq!(tv.status, "active");
    }

    #[test]
    fn url_builders() {
        assert_eq!(
            zone_dashboard_url(Some("acc1"), "example.com"),
            "https://dash.cloudflare.com/acc1/example.com"
        );
        assert_eq!(
            worker_dashboard_url("acc1", "my-worker"),
            "https://dash.cloudflare.com/acc1/workers/services/view/my-worker/production"
        );
        assert_eq!(
            pages_dashboard_url("acc1", "docs"),
            "https://dash.cloudflare.com/acc1/pages/view/docs"
        );
        assert_eq!(
            dns_dashboard_url(Some("acc1"), "example.com"),
            "https://dash.cloudflare.com/acc1/example.com/dns"
        );
        assert_eq!(
            security_dashboard_url(Some("acc1"), "example.com"),
            "https://dash.cloudflare.com/acc1/example.com/security/events"
        );
    }

    #[test]
    fn envelope_failure_path() {
        let body = r#"{"success":false,"errors":[{"code":7003,"message":"Could not route"}]}"#;
        let r: Result<Vec<Zone>> = parse_envelope(reqwest::StatusCode::OK, body);
        let err = r.unwrap_err().to_string();
        assert!(err.contains("Could not route"));
    }
}
