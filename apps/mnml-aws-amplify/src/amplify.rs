//! Thin wrappers around `aws amplify list-apps` / `list-branches` /
//! `list-jobs`. Each one is a one-shot subprocess; the App invokes
//! them on a worker thread and drains the result channel each tick.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AmplifyApp {
    pub app_id: String,
    pub name: String,
    pub default_domain: Option<String>,
    pub repository: Option<String>,
    pub platform: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AmplifyBranch {
    pub branch_name: String,
    pub stage: Option<String>,
    pub display_name: Option<String>,
    /// e.g. `https://<branch>.<app_id>.amplifyapp.com`. Built from
    /// the app's defaultDomain when absent. Reserved for v0.2.
    pub url: Option<String>,
    pub active_job_id: Option<String>,
    /// AWS Amplify's `updateTime` on the branch. Populated when the
    /// API returns it (usually the last-deploy or last-config-edit
    /// timestamp). Powers the staleness filter that hides feature
    /// branches whose last activity is beyond STALE_AFTER_DAYS.
    /// tree-redesign 2026-07-20.
    pub update_time: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AmplifyJob {
    pub job_id: String,
    pub status: String,
    /// PROVISION / BUILD / DEPLOY / VERIFY — set when status is in
    /// progress. None when the job is finished.
    pub current_step: Option<String>,
    pub commit_id: Option<String>,
    pub commit_message: Option<String>,
    /// Reserved for v0.2 rendering (currently only commit_id +
    /// commit_message + status are shown).
    pub commit_time: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AmplifyJobStep {
    pub step_name: String,
    pub status: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    /// S3 pre-signed URL for the step's log. Fetch with `fetch_log`.
    pub log_url: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AmplifyJobDetail {
    pub job_id: String,
    pub status: String,
    pub branch_name: String,
    pub commit_id: Option<String>,
    pub commit_message: Option<String>,
    pub steps: Vec<AmplifyJobStep>,
}

#[derive(Debug, Clone)]
pub enum AmplifyEvent {
    Apps(Vec<AmplifyApp>),
    Branches(Vec<AmplifyBranch>),
    /// Jobs for one branch — carries the branch name so a single
    /// receiver channel can multiplex per-branch fetches. Used
    /// both for the "latest 2 per branch" inline sweep and for
    /// user-triggered refreshes.
    Jobs {
        branch_name: String,
        jobs: Vec<AmplifyJob>,
    },
    JobDetail(AmplifyJobDetail),
    /// Log fetched from an S3 pre-signed URL. Carries the step
    /// name so multiple parallel fetches can be routed at the UI
    /// layer.
    Log {
        step_name: String,
        text: String,
    },
    Failed(String),
}

/// `aws amplify list-apps` — returns every Amplify app the
/// authenticated principal can see.
pub fn spawn_list_apps(region: Option<String>) -> Receiver<AmplifyEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let result = list_apps(region.as_deref());
        let _ = match result {
            Ok(apps) => tx.send(AmplifyEvent::Apps(apps)),
            Err(e) => tx.send(AmplifyEvent::Failed(e.to_string())),
        };
    });
    rx
}

pub fn spawn_list_branches(app_id: String, region: Option<String>) -> Receiver<AmplifyEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let result = list_branches(&app_id, region.as_deref());
        let _ = match result {
            Ok(branches) => tx.send(AmplifyEvent::Branches(branches)),
            Err(e) => tx.send(AmplifyEvent::Failed(e.to_string())),
        };
    });
    rx
}

pub fn spawn_list_jobs(
    app_id: String,
    branch_name: String,
    region: Option<String>,
) -> Receiver<AmplifyEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let result = list_jobs(&app_id, &branch_name, region.as_deref());
        let _ = match result {
            Ok(jobs) => tx.send(AmplifyEvent::Jobs { branch_name, jobs }),
            Err(e) => tx.send(AmplifyEvent::Failed(e.to_string())),
        };
    });
    rx
}

/// `aws amplify get-job` — full detail for a single job including
/// steps + per-step log URLs. Backing the Enter-to-drill-in flow.
pub fn spawn_get_job(
    app_id: String,
    branch_name: String,
    job_id: String,
    region: Option<String>,
) -> Receiver<AmplifyEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let result = get_job(&app_id, &branch_name, &job_id, region.as_deref());
        let _ = match result {
            Ok(detail) => tx.send(AmplifyEvent::JobDetail(detail)),
            Err(e) => tx.send(AmplifyEvent::Failed(e.to_string())),
        };
    });
    rx
}

/// Fetch a step's log from its S3 pre-signed URL. Used by the
/// drill-in logs viewer.
pub fn spawn_fetch_log(step_name: String, url: String) -> Receiver<AmplifyEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let result = fetch_log(&url);
        let _ = match result {
            Ok(text) => tx.send(AmplifyEvent::Log { step_name, text }),
            Err(e) => tx.send(AmplifyEvent::Failed(format!("log fetch {step_name}: {e}"))),
        };
    });
    rx
}

fn list_apps(region: Option<&str>) -> Result<Vec<AmplifyApp>> {
    let json = run_aws(&["amplify", "list-apps", "--max-results", "100"], region)?;
    let raw: AppsResponse =
        serde_json::from_value(json).context("parse amplify list-apps response")?;
    Ok(raw
        .apps
        .into_iter()
        .map(|a| AmplifyApp {
            app_id: a.app_id,
            name: a.name,
            default_domain: a.default_domain,
            repository: a.repository,
            platform: a.platform,
        })
        .collect())
}

fn list_branches(app_id: &str, region: Option<&str>) -> Result<Vec<AmplifyBranch>> {
    // AWS caps ListBranches at 50; passing 100 fails with
    // ValidationException. (list-apps, in contrast, caps at 100.)
    let json = run_aws(
        &[
            "amplify",
            "list-branches",
            "--app-id",
            app_id,
            "--max-results",
            "50",
        ],
        region,
    )?;
    let raw: BranchesResponse =
        serde_json::from_value(json).context("parse amplify list-branches response")?;
    let default_domain = None::<String>;
    Ok(raw
        .branches
        .into_iter()
        .map(|b| {
            let url = if let Some(d) = &default_domain {
                Some(format!("https://{}.{}", b.branch_name, d))
            } else {
                None
            };
            // AWS returns updateTime as a float epoch-seconds, or
            // sometimes as an ISO string on newer API versions.
            // Coerce both into an ISO-8601 timestamp so the
            // downstream days_since parser has one shape to eat.
            let update_time = b.update_time.and_then(|v| match v {
                serde_json::Value::Number(n) => {
                    let epoch = n.as_f64()? as i64;
                    Some(epoch_to_iso(epoch))
                }
                serde_json::Value::String(s) => Some(s),
                _ => None,
            });
            AmplifyBranch {
                branch_name: b.branch_name,
                stage: b.stage,
                display_name: b.display_name,
                url,
                active_job_id: b.active_job_id,
                update_time,
            }
        })
        .collect())
}

/// Convert Unix epoch seconds to a synthetic ISO-8601 UTC string
/// (`YYYY-MM-DDT00:00:00Z`). Only the date portion is used by the
/// downstream staleness filter, so we don't need HMS precision.
fn epoch_to_iso(epoch: i64) -> String {
    // Days since Unix epoch → Julian date → gregorian date.
    let days = epoch / 86_400;
    let jd = 2440588 + days;
    let l = jd + 68_569;
    let n = 4 * l / 146_097;
    let l = l - (146_097 * n + 3) / 4;
    let i = 4000 * (l + 1) / 1_461_001;
    let l = l - 1461 * i / 4 + 31;
    let j = 80 * l / 2447;
    let d = l - 2447 * j / 80;
    let l = j / 11;
    let m = j + 2 - 12 * l;
    let y = 100 * (n - 49) + i + l;
    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}

fn list_jobs(app_id: &str, branch_name: &str, region: Option<&str>) -> Result<Vec<AmplifyJob>> {
    let json = run_aws(
        &[
            "amplify",
            "list-jobs",
            "--app-id",
            app_id,
            "--branch-name",
            branch_name,
            "--max-results",
            "50",
        ],
        region,
    )?;
    let raw: JobsResponse =
        serde_json::from_value(json).context("parse amplify list-jobs response")?;
    Ok(raw
        .job_summaries
        .into_iter()
        .map(|j| AmplifyJob {
            job_id: j.job_id,
            status: j.status,
            current_step: j.current_step,
            commit_id: j.commit_id,
            commit_message: j.commit_message,
            commit_time: j.commit_time,
            start_time: j.start_time,
            end_time: j.end_time,
        })
        .collect())
}

fn get_job(
    app_id: &str,
    branch_name: &str,
    job_id: &str,
    region: Option<&str>,
) -> Result<AmplifyJobDetail> {
    let json = run_aws(
        &[
            "amplify",
            "get-job",
            "--app-id",
            app_id,
            "--branch-name",
            branch_name,
            "--job-id",
            job_id,
        ],
        region,
    )?;
    let raw: JobDetailResponse =
        serde_json::from_value(json).context("parse amplify get-job response")?;
    let summary = raw.job.summary;
    Ok(AmplifyJobDetail {
        job_id: summary.job_id,
        status: summary.status,
        branch_name: branch_name.to_string(),
        commit_id: summary.commit_id,
        commit_message: summary.commit_message,
        steps: raw
            .job
            .steps
            .into_iter()
            .map(|s| AmplifyJobStep {
                step_name: s.step_name,
                status: s.status,
                start_time: s.start_time,
                end_time: s.end_time,
                log_url: s.log_url,
            })
            .collect(),
    })
}

/// GET the log body from an S3 pre-signed URL. Amplify hands us
/// short-lived URLs; the fetch is one shot. Uses reqwest blocking
/// (already in tree for HTTPS). Trims oversize logs to 200KB —
/// Amplify build logs can be tens of MB and lock up the render
/// path.
fn fetch_log(url: &str) -> Result<String> {
    let body = reqwest::blocking::get(url)?.text()?;
    const MAX: usize = 200 * 1024;
    if body.len() > MAX {
        let head: String = body.chars().take(MAX).collect();
        Ok(format!("{head}\n\n… (truncated at {MAX} bytes)"))
    } else {
        Ok(body)
    }
}

fn run_aws(args: &[&str], region: Option<&str>) -> Result<serde_json::Value> {
    let mut cmd = Command::new("aws");
    if let Some(r) = region {
        cmd.arg("--region").arg(r);
    }
    cmd.args(args).arg("--output").arg("json");
    let out = cmd
        .output()
        .map_err(|e| anyhow!("spawn aws: {e} — is the AWS CLI on PATH?"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "aws {} → {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if out.stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("parse json: {e}"))
}

/// Console URL for an Amplify app (app-level view, no branch).
pub fn console_url_app(app_id: &str, region: Option<&str>) -> String {
    let r = region.unwrap_or("us-east-1");
    format!("https://{r}.console.aws.amazon.com/amplify/apps/{app_id}")
}

/// Console URL for a specific Amplify branch.
pub fn console_url_branch(app_id: &str, branch: &str, region: Option<&str>) -> String {
    let r = region.unwrap_or("us-east-1");
    format!("https://{r}.console.aws.amazon.com/amplify/apps/{app_id}/branches/{branch}")
}

// ─── Raw deserialization types ────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AppsResponse {
    #[serde(rename = "apps", default)]
    apps: Vec<RawApp>,
}

#[derive(Debug, Deserialize)]
struct RawApp {
    #[serde(rename = "appId")]
    app_id: String,
    name: String,
    #[serde(rename = "defaultDomain", default)]
    default_domain: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    platform: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BranchesResponse {
    #[serde(rename = "branches", default)]
    branches: Vec<RawBranch>,
}

#[derive(Debug, Deserialize)]
struct RawBranch {
    #[serde(rename = "branchName")]
    branch_name: String,
    #[serde(default)]
    stage: Option<String>,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(rename = "activeJobId", default)]
    active_job_id: Option<String>,
    #[serde(rename = "updateTime", default)]
    update_time: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JobsResponse {
    #[serde(rename = "jobSummaries", default)]
    job_summaries: Vec<RawJob>,
}

#[derive(Debug, Deserialize)]
struct JobDetailResponse {
    job: RawJobDetail,
}

#[derive(Debug, Deserialize)]
struct RawJobDetail {
    summary: RawJob,
    #[serde(default)]
    steps: Vec<RawJobStep>,
}

#[derive(Debug, Deserialize)]
struct RawJobStep {
    #[serde(rename = "stepName")]
    step_name: String,
    status: String,
    #[serde(rename = "startTime", default)]
    start_time: Option<String>,
    #[serde(rename = "endTime", default)]
    end_time: Option<String>,
    #[serde(rename = "logUrl", default)]
    log_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawJob {
    #[serde(rename = "jobId")]
    job_id: String,
    status: String,
    #[serde(rename = "jobType", default)]
    _job_type: Option<String>,
    #[serde(rename = "currentStep", default)]
    current_step: Option<String>,
    #[serde(rename = "commitId", default)]
    commit_id: Option<String>,
    #[serde(rename = "commitMessage", default)]
    commit_message: Option<String>,
    #[serde(rename = "commitTime", default)]
    commit_time: Option<String>,
    #[serde(rename = "startTime", default)]
    start_time: Option<String>,
    #[serde(rename = "endTime", default)]
    end_time: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_url_app_uses_region() {
        let url = console_url_app("d2abc123", Some("us-west-2"));
        assert!(url.contains("us-west-2.console.aws.amazon.com"));
        assert!(url.contains("apps/d2abc123"));
    }

    #[test]
    fn console_url_branch_includes_branch() {
        let url = console_url_branch("d2abc123", "main", Some("us-east-1"));
        assert!(url.contains("/branches/main"));
    }

    #[test]
    fn parses_minimal_apps_response() {
        let json = r##"{"apps": [
            {"appId": "d2abc", "name": "MyApp", "defaultDomain": "abc.amplifyapp.com",
             "repository": "https://github.com/x/y", "platform": "WEB"}
        ]}"##;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let r: AppsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r.apps.len(), 1);
        assert_eq!(r.apps[0].name, "MyApp");
    }
}
