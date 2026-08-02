//! `docker` CLI shell-outs + structured response models. Pure CLI —
//! no SDK dep. Mirrors the AWS-family pattern (each `*.rs` of an AWS
//! sibling shells out to `aws`; here it's `docker`).
//!
//! Docker's `--format '{{json .}}'` emits one JSON object per line
//! for list commands; `docker inspect <id>` emits a single-element
//! JSON array. We parse both.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::process::{Command, Output};

/// Markers the docker CLI returns in stderr when the daemon socket
/// isn't reachable. We sniff for any of these to surface a friendly
/// top-level "daemon offline" state.
///
/// - Classic Docker Desktop (pre-2025): `Cannot connect to the Docker daemon`.
/// - Modern docker CLI / colima / podman-docker shim:
///   `failed to connect to the docker API at unix://...`.
pub const DAEMON_OFFLINE_MARKERS: &[&str] = &[
    "Cannot connect to the Docker daemon",
    "failed to connect to the docker API",
];

/// Returns `true` when the given stderr text looks like the
/// daemon-not-running error.
pub fn is_daemon_offline(stderr: &str) -> bool {
    DAEMON_OFFLINE_MARKERS.iter().any(|m| stderr.contains(m))
}

/// Friendly error from a failed `docker ...` invocation. Trims and
/// prefixes the stderr to match the family convention.
fn docker_err(stderr: &[u8]) -> anyhow::Error {
    let s = String::from_utf8_lossy(stderr);
    anyhow!("docker: {}", s.trim())
}

/// Run a `docker` subcommand and return stdout on success. Failures
/// bubble up as the family-canonical "docker: <stderr>" error.
fn run_docker(args: &[&str]) -> Result<Vec<u8>> {
    let out: Output = Command::new("docker")
        .args(args)
        .output()
        .with_context(|| format!("spawn `docker {}`", args.join(" ")))?;
    if !out.status.success() {
        return Err(docker_err(&out.stderr));
    }
    Ok(out.stdout)
}

/// Run a `docker` subcommand returning success/failure-with-stderr
/// without the `docker:` prefix translation — used by the daemon
/// probe so it can sniff for `DAEMON_OFFLINE_MARKERS` raw.
pub fn run_docker_raw(args: &[&str]) -> Result<(bool, String, String)> {
    let out: Output = Command::new("docker")
        .args(args)
        .output()
        .with_context(|| format!("spawn `docker {}`", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), stdout, stderr))
}

/// Parse a stream of newline-delimited JSON objects.
fn parse_ndjson<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<Vec<T>> {
    let s = std::str::from_utf8(bytes).context("docker stdout was not UTF-8")?;
    let mut out = Vec::new();
    for (i, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: T = serde_json::from_str(line)
            .with_context(|| format!("parse docker ndjson line {}", i + 1))?;
        out.push(v);
    }
    Ok(out)
}

// ─── Containers ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Container {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Image", default)]
    pub image: String,
    #[serde(rename = "Names", default)]
    pub names: String,
    #[serde(rename = "Status", default)]
    pub status: String,
    #[serde(rename = "State", default)]
    pub state: String,
    #[serde(rename = "Ports", default)]
    pub ports: String,
    #[serde(rename = "RunningFor", default)]
    pub running_for: String,
    #[serde(rename = "Command", default)]
    pub command: String,
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
}

impl Container {
    /// 12-char short form of the container ID — same convention
    /// `docker ps` uses for display.
    pub fn short_id(&self) -> &str {
        let cap = self.id.len().min(12);
        &self.id[..cap]
    }

    /// Container `State` field from `docker ps --format '{{json .}}'`
    /// — one of `running` / `exited` / `created` / `restarting` /
    /// `paused` / `dead` / `removing`.
    pub fn is_running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }
}

/// `docker ps -a --format '{{json .}}'` — every container, including
/// stopped ones. Returns oldest-first; we sort by name client-side
/// for stable display.
pub fn list_containers() -> Result<Vec<Container>> {
    let bytes = run_docker(&["ps", "-a", "--format", "{{json .}}"])?;
    let mut all: Vec<Container> = parse_ndjson(&bytes)?;
    all.sort_by_key(|c| c.names.to_lowercase());
    Ok(all)
}

/// `docker inspect <id>` — pretty-printed JSON detail. Returns the
/// raw stdout so the UI can paint it verbatim.
pub fn inspect(id: &str) -> Result<String> {
    let bytes = run_docker(&["inspect", id])?;
    let s = std::str::from_utf8(&bytes)
        .context("docker inspect stdout was not UTF-8")?
        .to_string();
    Ok(s)
}

// ─── Images ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Image {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Repository", default)]
    pub repository: String,
    #[serde(rename = "Tag", default)]
    pub tag: String,
    #[serde(rename = "Size", default)]
    pub size: String,
    #[serde(rename = "CreatedSince", default)]
    pub created_since: String,
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
    #[serde(rename = "Digest", default)]
    pub digest: String,
}

impl Image {
    pub fn short_id(&self) -> &str {
        // Image IDs from `docker images --format` are already short
        // (sha256-stripped), but be defensive.
        let raw = self.id.strip_prefix("sha256:").unwrap_or(&self.id);
        let cap = raw.len().min(12);
        &raw[..cap]
    }

    pub fn repo_tag(&self) -> String {
        if self.repository.is_empty() && self.tag.is_empty() {
            "<none>:<none>".into()
        } else if self.tag.is_empty() {
            self.repository.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }
}

pub fn list_images() -> Result<Vec<Image>> {
    let bytes = run_docker(&["images", "--format", "{{json .}}"])?;
    let mut all: Vec<Image> = parse_ndjson(&bytes)?;
    all.sort_by_key(|i| i.repo_tag().to_lowercase());
    Ok(all)
}

// ─── Volumes ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Volume {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Driver", default)]
    pub driver: String,
    #[serde(rename = "Mountpoint", default)]
    pub mountpoint: String,
    #[serde(rename = "Scope", default)]
    pub scope: String,
}

pub fn list_volumes() -> Result<Vec<Volume>> {
    let bytes = run_docker(&["volume", "ls", "--format", "{{json .}}"])?;
    let mut all: Vec<Volume> = parse_ndjson(&bytes)?;
    all.sort_by_key(|v| v.name.to_lowercase());
    Ok(all)
}

pub fn inspect_volume(name: &str) -> Result<String> {
    let bytes = run_docker(&["volume", "inspect", name])?;
    let s = std::str::from_utf8(&bytes)
        .context("docker volume inspect stdout was not UTF-8")?
        .to_string();
    Ok(s)
}

// ─── Networks ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Network {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Driver", default)]
    pub driver: String,
    #[serde(rename = "Scope", default)]
    pub scope: String,
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
}

impl Network {
    pub fn short_id(&self) -> &str {
        let cap = self.id.len().min(12);
        &self.id[..cap]
    }
}

pub fn list_networks() -> Result<Vec<Network>> {
    let bytes = run_docker(&["network", "ls", "--format", "{{json .}}"])?;
    let mut all: Vec<Network> = parse_ndjson(&bytes)?;
    all.sort_by_key(|n| n.name.to_lowercase());
    Ok(all)
}

pub fn inspect_network(name: &str) -> Result<String> {
    let bytes = run_docker(&["network", "inspect", name])?;
    let s = std::str::from_utf8(&bytes)
        .context("docker network inspect stdout was not UTF-8")?
        .to_string();
    Ok(s)
}

// ─── Compose ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ComposeService {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Service", default)]
    pub service: String,
    #[serde(rename = "State", default)]
    pub state: String,
    #[serde(rename = "Status", default)]
    pub status: String,
    #[serde(rename = "Image", default, alias = "Image")]
    pub image: String,
    #[serde(rename = "Project", default)]
    pub project: String,
}

/// `docker compose -f <path> ps --format json` — the v2 compose CLI
/// returns a JSON *array* (not ndjson — that's the divergence from
/// the other docker list commands). We accept both shapes for safety.
pub fn list_compose_services(compose_file: &str) -> Result<Vec<ComposeService>> {
    let bytes = run_docker(&["compose", "-f", compose_file, "ps", "--format", "json"])?;
    parse_compose_ps(&bytes)
}

fn parse_compose_ps(bytes: &[u8]) -> Result<Vec<ComposeService>> {
    let trimmed = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|i| &bytes[i..])
        .unwrap_or(&[]);
    if trimmed.first() == Some(&b'[') {
        // JSON array shape.
        let arr: Vec<ComposeService> =
            serde_json::from_slice(bytes).context("parse compose ps JSON array")?;
        Ok(arr)
    } else {
        // ndjson shape — also tolerated by older compose builds.
        parse_ndjson(bytes)
    }
}

// ─── Actions ─────────────────────────────────────────────────────────

pub fn stop_container(id: &str) -> Result<()> {
    run_docker(&["stop", id]).map(|_| ())
}

pub fn start_container(id: &str) -> Result<()> {
    run_docker(&["start", id]).map(|_| ())
}

pub fn rm_container(id: &str) -> Result<()> {
    run_docker(&["rm", "-f", id]).map(|_| ())
}

pub fn rmi_image(id: &str) -> Result<()> {
    run_docker(&["rmi", id]).map(|_| ())
}

pub fn rm_volume(name: &str) -> Result<()> {
    run_docker(&["volume", "rm", name]).map(|_| ())
}

pub fn rm_network(name: &str) -> Result<()> {
    run_docker(&["network", "rm", name]).map(|_| ())
}

// ─── ECR URL detection ──────────────────────────────────────────────

/// AWS ECR repository URLs look like:
///   <acct>.dkr.ecr.<region>.amazonaws.com/<repo>[:tag]
///
/// Returns `Some((account, region))` when `image_ref` looks like an
/// ECR image reference, `None` otherwise. The repo + tag portion is
/// not needed for the cross-sibling jump (mnml-aws-ecr opens scoped
/// by region, not by repo — at least in v0.1).
pub fn parse_ecr_url(image_ref: &str) -> Option<(String, String)> {
    // Take everything before the first `/` — that's the registry host.
    let host = image_ref.split('/').next()?;
    // Expected segments: <acct> "dkr" "ecr" <region> "amazonaws" "com"
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 6 {
        return None;
    }
    if parts[1] != "dkr" || parts[2] != "ecr" || parts[4] != "amazonaws" || parts[5] != "com" {
        return None;
    }
    let acct = parts[0];
    let region = parts[3];
    if acct.is_empty() || region.is_empty() {
        return None;
    }
    // Sanity: AWS account IDs are 12 numeric digits.
    if !acct.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((acct.to_string(), region.to_string()))
}

// ─── Daemon probe ────────────────────────────────────────────────────

/// Result of probing whether the docker daemon is reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// Daemon answered — `docker version`'s server version (or a
    /// short fallback string) for the status bar.
    Ok(String),
    /// Daemon socket isn't reachable.
    Offline,
    /// `docker` CLI isn't on PATH at all.
    CliMissing(String),
    /// Some other error from the CLI.
    Error(String),
}

/// Calls `docker version --format '{{.Server.Version}}'` and
/// interprets the outcome.
pub fn probe_daemon() -> DaemonState {
    let res = run_docker_raw(&["version", "--format", "{{.Server.Version}}"]);
    match res {
        Ok((true, stdout, _)) => {
            let v = stdout.trim();
            if v.is_empty() {
                DaemonState::Ok("unknown".into())
            } else {
                DaemonState::Ok(v.to_string())
            }
        }
        Ok((false, _, stderr)) => {
            if is_daemon_offline(&stderr) {
                DaemonState::Offline
            } else {
                DaemonState::Error(stderr.trim().to_string())
            }
        }
        Err(e) => DaemonState::CliMissing(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_container_ndjson() {
        let s = r#"{"ID":"abc1234def56","Image":"redis:7","Names":"redis","Status":"Up 2 hours","State":"running","Ports":"6379/tcp","RunningFor":"2 hours ago","Command":"redis-server","CreatedAt":"2026-06-07 12:00:00 +0000 UTC"}
{"ID":"99887766aabb","Image":"postgres:16","Names":"pg","Status":"Exited (0)","State":"exited","Ports":"","RunningFor":"3 days ago","Command":"postgres","CreatedAt":"2026-06-04 09:00:00 +0000 UTC"}
"#;
        let out: Vec<Container> = parse_ndjson(s.as_bytes()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].short_id(), "abc1234def56");
        assert!(out[0].is_running());
        assert!(!out[1].is_running());
        assert_eq!(out[1].image, "postgres:16");
    }

    #[test]
    fn parses_image_ndjson_and_repo_tag() {
        let s = r#"{"ID":"sha256:0123456789abcdef","Repository":"redis","Tag":"7","Size":"110MB","CreatedSince":"3 weeks ago","CreatedAt":"2026-05-17 ...","Digest":""}
{"ID":"abcabcabc","Repository":"<none>","Tag":"<none>","Size":"42MB","CreatedSince":"1 hour ago","CreatedAt":"2026-06-07 ...","Digest":""}
"#;
        let out: Vec<Image> = parse_ndjson(s.as_bytes()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].short_id(), "0123456789ab");
        assert_eq!(out[0].repo_tag(), "redis:7");
        assert_eq!(out[1].repo_tag(), "<none>:<none>");
    }

    #[test]
    fn parses_volume_ndjson() {
        let s = r#"{"Name":"pgdata","Driver":"local","Mountpoint":"/var/lib/docker/volumes/pgdata/_data","Scope":"local"}
{"Name":"redis","Driver":"local","Mountpoint":"/var/lib/docker/volumes/redis/_data","Scope":"local"}
"#;
        let out: Vec<Volume> = parse_ndjson(s.as_bytes()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "pgdata");
        assert_eq!(out[0].driver, "local");
    }

    #[test]
    fn parses_network_ndjson() {
        let s = r#"{"ID":"abc111222333","Name":"bridge","Driver":"bridge","Scope":"local","CreatedAt":"2026-06-01 ..."}
{"ID":"def444555666","Name":"host","Driver":"host","Scope":"local","CreatedAt":"2026-06-01 ..."}
"#;
        let out: Vec<Network> = parse_ndjson(s.as_bytes()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "bridge");
        assert_eq!(out[0].short_id(), "abc111222333");
    }

    #[test]
    fn parses_compose_ps_json_array() {
        let s = r#"[
            {"Name":"myapp-web-1","Service":"web","State":"running","Status":"Up","Image":"myorg/web:latest","Project":"myapp"},
            {"Name":"myapp-db-1","Service":"db","State":"exited","Status":"Exited (0)","Image":"postgres:16","Project":"myapp"}
        ]"#;
        let out = parse_compose_ps(s.as_bytes()).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].service, "web");
        assert_eq!(out[1].state, "exited");
    }

    #[test]
    fn parses_compose_ps_ndjson_fallback() {
        // Older compose CLI builds emitted ndjson — make sure we
        // still accept that shape.
        let s = r#"{"Name":"x-web-1","Service":"web","State":"running","Status":"Up","Image":"x:latest","Project":"x"}
{"Name":"x-db-1","Service":"db","State":"running","Status":"Up","Image":"postgres:16","Project":"x"}
"#;
        let out = parse_compose_ps(s.as_bytes()).unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn ecr_url_detected() {
        let got = parse_ecr_url("123456789012.dkr.ecr.us-east-1.amazonaws.com/my-app:v1.2.3");
        assert_eq!(
            got,
            Some(("123456789012".to_string(), "us-east-1".to_string()))
        );
    }

    #[test]
    fn ecr_url_non_ecr_rejected() {
        assert_eq!(parse_ecr_url("redis:7"), None);
        assert_eq!(parse_ecr_url("docker.io/library/redis:7"), None);
        assert_eq!(
            parse_ecr_url("public.ecr.aws/lts/ubuntu:22.04"),
            None,
            "ECR Public is a different surface (public.ecr.aws/...)"
        );
    }

    #[test]
    fn ecr_url_malformed_rejected() {
        // Wrong segment count.
        assert_eq!(
            parse_ecr_url("123456789012.dkr.ecr.us-east-1.amazonaws/my-app"),
            None
        );
        // Non-numeric account.
        assert_eq!(
            parse_ecr_url("notanaccount.dkr.ecr.us-east-1.amazonaws.com/my-app"),
            None
        );
        // Empty input.
        assert_eq!(parse_ecr_url(""), None);
    }

    #[test]
    fn daemon_offline_marker_sniff() {
        // Classic Docker Desktop wording.
        let s = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";
        assert!(is_daemon_offline(s));
        // Modern docker CLI wording (Docker 24+, colima, podman-docker shim).
        let s2 = "failed to connect to the docker API at unix:///nonexistent.sock; check if the path is correct and if the daemon is running";
        assert!(is_daemon_offline(s2));
        assert!(!is_daemon_offline("some unrelated docker error"));
    }
}
