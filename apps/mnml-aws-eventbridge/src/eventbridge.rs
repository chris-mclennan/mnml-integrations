//! AWS EventBridge Scheduler client — shells out to `aws scheduler`.
//! Only three verbs: list-schedules, get-schedule, update-schedule.
//!
//! 2026-07-21 rewrite. The previous version wrapped `aws events`
//! (the older Rules service); user wanted a focused Schedules
//! browser + editor instead. See README.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::process::Command;

/// One row in the list — cheap shape returned by `list-schedules`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)]
pub struct ScheduleSummary {
    pub name: String,
    pub group_name: String,
    pub arn: String,
    pub state: String,
    #[serde(default)]
    pub target: Option<TargetSummary>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)]
pub struct TargetSummary {
    pub arn: String,
}

/// Full schedule — returned by `get-schedule` and passed back to
/// `update-schedule`. We deserialize the fields we render / edit
/// and keep the rest verbatim via `raw` so an update doesn't
/// strip fields we don't know about.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScheduleDetail {
    pub name: String,
    pub group_name: String,
    pub description: String,
    pub state: String,
    pub schedule_expression: String,
    pub schedule_expression_timezone: String,
    pub flexible_time_window: serde_json::Value,
    pub target: ScheduleTarget,
    /// Full JSON returned by get-schedule; used as the merge base
    /// for update-schedule so uncommon fields (kms key, start/end
    /// dates, retry policy) survive the round trip.
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScheduleTarget {
    pub arn: String,
    pub role_arn: String,
    #[serde(default)]
    pub input: Option<String>,
}

fn run_aws(args: &[&str], region: Option<&str>) -> Result<serde_json::Value> {
    let mut cmd = Command::new("aws");
    cmd.arg("scheduler");
    for a in args {
        cmd.arg(a);
    }
    if let Some(r) = region {
        cmd.args(["--region", r]);
    }
    cmd.args(["--output", "json"]);
    let out = cmd
        .output()
        .with_context(|| format!("spawn aws scheduler {}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        bail!("aws scheduler {}: {stderr}", args.join(" "));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&stdout).with_context(|| format!("parse aws scheduler {}", args.join(" ")))
}

pub fn list_schedules(region: Option<&str>) -> Result<Vec<ScheduleSummary>> {
    // Follow NextToken pagination — Scheduler caps each response
    // at 100 (default) or up to 200 with --max-results. Accounts
    // with more schedules silently lost the tail before this
    // loop landed (2026-07-22 tester finding).
    let mut items: Vec<ScheduleSummary> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut args: Vec<String> = vec![
            "list-schedules".into(),
            "--max-results".into(),
            "100".into(),
        ];
        if let Some(tok) = &next_token {
            args.push("--next-token".into());
            args.push(tok.clone());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let v = run_aws(&arg_refs, region)?;
        if let Some(arr) = v.get("Schedules").cloned() {
            let page: Vec<ScheduleSummary> =
                serde_json::from_value(arr).context("parse list-schedules Schedules[]")?;
            items.extend(page);
        }
        next_token = v
            .get("NextToken")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        if next_token.is_none() {
            break;
        }
    }
    Ok(items)
}

pub fn get_schedule(name: &str, group_name: &str, region: Option<&str>) -> Result<ScheduleDetail> {
    let v = run_aws(
        &["get-schedule", "--name", name, "--group-name", group_name],
        region,
    )?;
    let name = v
        .get("Name")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let group_name = v
        .get("GroupName")
        .and_then(|x| x.as_str())
        .unwrap_or("default")
        .to_string();
    let description = v
        .get("Description")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let state = v
        .get("State")
        .and_then(|x| x.as_str())
        .unwrap_or("ENABLED")
        .to_string();
    let schedule_expression = v
        .get("ScheduleExpression")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let schedule_expression_timezone = v
        .get("ScheduleExpressionTimezone")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let flexible_time_window = v
        .get("FlexibleTimeWindow")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"Mode":"OFF"}));
    let target: ScheduleTarget = v
        .get("Target")
        .cloned()
        .map(serde_json::from_value)
        .ok_or_else(|| anyhow::anyhow!("get-schedule: missing Target"))??;
    Ok(ScheduleDetail {
        name,
        group_name,
        description,
        state,
        schedule_expression,
        schedule_expression_timezone,
        flexible_time_window,
        target,
        raw: v,
    })
}

/// Update a schedule with a new expression + target input. Merges
/// over the stashed `raw` blob so unknown fields survive.
pub fn update_schedule(
    detail: &ScheduleDetail,
    new_expression: &str,
    new_target_input: &str,
    region: Option<&str>,
) -> Result<()> {
    submit_update(detail, region, |map| {
        map.insert(
            "ScheduleExpression".to_string(),
            serde_json::Value::String(new_expression.to_string()),
        );
        if let Some(serde_json::Value::Object(t)) = map.get_mut("Target") {
            t.insert(
                "Input".to_string(),
                serde_json::Value::String(new_target_input.to_string()),
            );
        }
    })
}

/// Flip a schedule between ENABLED and DISABLED — a bare
/// `update-schedule` with just the state changed, everything else
/// preserved via the raw-blob merge. Returns the new state ("ENABLED"
/// or "DISABLED").
pub fn toggle_state(detail: &ScheduleDetail, region: Option<&str>) -> Result<String> {
    let next = if detail.state == "ENABLED" {
        "DISABLED"
    } else {
        "ENABLED"
    };
    let next_owned = next.to_string();
    submit_update(detail, region, |map| {
        map.insert(
            "State".to_string(),
            serde_json::Value::String(next_owned.clone()),
        );
    })?;
    Ok(next.to_string())
}

/// Common body: clone `raw`, apply the mutator, strip read-only
/// fields, ship via `--cli-input-json file://…` so multi-line JSON
/// bodies don't need shell escaping.
fn submit_update<F>(detail: &ScheduleDetail, region: Option<&str>, mutate: F) -> Result<()>
where
    F: FnOnce(&mut serde_json::Map<String, serde_json::Value>),
{
    let mut payload = detail.raw.clone();
    if let serde_json::Value::Object(map) = &mut payload {
        mutate(map);
        for k in ["Arn", "CreationDate", "LastModificationDate"] {
            map.remove(k);
        }
    }
    let dir = std::env::temp_dir();
    let path = dir.join(format!("mnml-scheduler-{}.json", std::process::id()));
    std::fs::write(&path, serde_json::to_string(&payload)?)?;

    let path_str = path.to_string_lossy().to_string();
    let file_arg = format!("file://{path_str}");
    let mut cmd = Command::new("aws");
    cmd.args([
        "scheduler",
        "update-schedule",
        "--cli-input-json",
        &file_arg,
    ]);
    if let Some(r) = region {
        cmd.args(["--region", r]);
    }
    cmd.args(["--output", "json"]);
    let out = cmd
        .output()
        .context("spawn aws scheduler update-schedule")?;
    let _ = std::fs::remove_file(&path);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        bail!("update-schedule: {stderr}");
    }
    Ok(())
}
