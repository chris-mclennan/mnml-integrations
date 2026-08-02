//! `aws lambda list-functions` / `get-function` shell-outs + the
//! structured response model. Pure CLI — no SDK dep.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Subset of the LambdaFunction shape returned by
/// `aws lambda list-functions` / `aws lambda get-function-configuration`.
/// Field names match the AWS API exactly so `serde_json` parses raw
/// CLI output without renames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Function {
    #[serde(rename = "FunctionName")]
    pub function_name: String,
    #[serde(rename = "FunctionArn", default)]
    pub function_arn: String,
    #[serde(rename = "Runtime", default)]
    pub runtime: Option<String>,
    #[serde(rename = "Handler", default)]
    pub handler: Option<String>,
    #[serde(rename = "Role", default)]
    pub role: String,
    #[serde(rename = "MemorySize", default)]
    pub memory_size: Option<u32>,
    #[serde(rename = "Timeout", default)]
    pub timeout: Option<u32>,
    #[serde(rename = "LastModified", default)]
    pub last_modified: Option<String>,
    #[serde(rename = "CodeSize", default)]
    pub code_size: Option<u64>,
    #[serde(rename = "Description", default)]
    pub description: Option<String>,
    #[serde(rename = "PackageType", default)]
    pub package_type: Option<String>,
    #[serde(rename = "Architectures", default)]
    pub architectures: Vec<String>,
    #[serde(rename = "Environment", default)]
    pub environment: Option<EnvironmentResponse>,
    #[serde(rename = "ReservedConcurrentExecutions", default)]
    pub reserved_concurrent_executions: Option<u32>,
    #[serde(rename = "TracingConfig", default)]
    pub tracing_config: Option<TracingConfig>,
    #[serde(rename = "DeadLetterConfig", default)]
    pub dead_letter_config: Option<DeadLetterConfig>,
}

/// The `Environment` block of `get-function-configuration` / `list-functions`.
/// We don't render the actual values (they're typically secrets); just the
/// count + any error from the resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentResponse {
    #[serde(rename = "Variables", default)]
    pub variables: std::collections::HashMap<String, String>,
    #[serde(rename = "Error", default)]
    pub error: Option<EnvironmentError>,
}

impl EnvironmentResponse {
    pub fn var_count(&self) -> usize {
        self.variables.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentError {
    #[serde(rename = "ErrorCode", default)]
    pub error_code: Option<String>,
    #[serde(rename = "Message", default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    #[serde(rename = "Mode", default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterConfig {
    #[serde(rename = "TargetArn", default)]
    pub target_arn: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListFunctionsResponse {
    #[serde(rename = "Functions")]
    functions: Vec<Function>,
    #[serde(rename = "NextMarker", default)]
    next_marker: Option<String>,
}

/// Run `aws lambda list-functions`. Paginates until exhaustion.
/// Returns functions sorted by name (case-insensitive).
pub fn list_functions(region: Option<&str>) -> Result<Vec<Function>> {
    let mut all = Vec::new();
    let mut marker: Option<String> = None;

    loop {
        let mut cmd = Command::new("aws");
        cmd.args(["lambda", "list-functions", "--output", "json"]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(m) = &marker {
            cmd.args(["--starting-token", m]);
        }

        let output = cmd
            .output()
            .with_context(|| "spawn `aws lambda list-functions`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws lambda list-functions failed: {}",
                stderr.trim()
            ));
        }

        let resp: ListFunctionsResponse =
            serde_json::from_slice(&output.stdout).with_context(|| "parse list-functions JSON")?;
        all.extend(resp.functions);

        match resp.next_marker {
            Some(m) if !m.is_empty() => marker = Some(m),
            _ => break,
        }
    }

    all.sort_by_key(|a| a.function_name.to_lowercase());
    Ok(all)
}

/// Fetch one function's configuration via `aws lambda
/// get-function-configuration`. Used for the focused-detail
/// panel + watched-tab loading.
pub fn get_function(name: &str, region: Option<&str>) -> Result<Function> {
    let mut cmd = Command::new("aws");
    cmd.args([
        "lambda",
        "get-function-configuration",
        "--function-name",
        name,
        "--output",
        "json",
    ]);
    if let Some(r) = region {
        cmd.args(["--region", r]);
    }
    let output = cmd
        .output()
        .with_context(|| format!("spawn `aws lambda get-function-configuration` for {name}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "aws lambda get-function-configuration failed for {name}: {}",
            stderr.trim()
        ));
    }
    let fun: Function = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse get-function-configuration JSON for {name}"))?;
    Ok(fun)
}

/// CloudWatch log group name convention for a Lambda function.
/// Used to hand off to `mnml-aws-cloudwatch-logs`.
pub fn log_group_for(name: &str) -> String {
    format!("/aws/lambda/{name}")
}

/// Format bytes as a short human-readable string (e.g. "1.2 MB").
pub fn fmt_bytes(n: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * 1024;
    const G: u64 = M * 1024;
    if n >= G {
        format!("{:.1} GB", n as f64 / G as f64)
    } else if n >= M {
        format!("{:.1} MB", n as f64 / M as f64)
    } else if n >= K {
        format!("{:.1} KB", n as f64 / K as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_functions_response() {
        let json = r#"
        {
            "Functions": [
                {
                    "FunctionName": "api-handler",
                    "FunctionArn": "arn:aws:lambda:us-east-1:111111111111:function:api-handler",
                    "Runtime": "nodejs20.x",
                    "Handler": "index.handler",
                    "Role": "arn:aws:iam::111111111111:role/lambda-role",
                    "MemorySize": 512,
                    "Timeout": 30,
                    "LastModified": "2026-06-02T12:34:56.000+0000",
                    "CodeSize": 1234567,
                    "Description": "API entry point",
                    "PackageType": "Zip",
                    "Architectures": ["arm64"]
                }
            ]
        }"#;
        let resp: ListFunctionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.functions.len(), 1);
        let f = &resp.functions[0];
        assert_eq!(f.function_name, "api-handler");
        assert_eq!(f.runtime.as_deref(), Some("nodejs20.x"));
        assert_eq!(f.memory_size, Some(512));
        assert_eq!(f.timeout, Some(30));
        assert_eq!(f.architectures, vec!["arm64"]);
    }

    #[test]
    fn parses_list_functions_with_pagination_marker() {
        let json = r#"{"Functions": [], "NextMarker": "tok123"}"#;
        let resp: ListFunctionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.next_marker.as_deref(), Some("tok123"));
    }

    #[test]
    fn log_group_for_uses_aws_lambda_prefix() {
        assert_eq!(log_group_for("my-func"), "/aws/lambda/my-func");
    }

    #[test]
    fn fmt_bytes_picks_right_unit() {
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(3_145_728), "3.0 MB");
    }

    #[test]
    fn parses_environment_variables() {
        let json = r#"
        {
            "Functions": [
                {
                    "FunctionName": "api-handler",
                    "FunctionArn": "arn:aws:lambda:us-east-1:1:function:api-handler",
                    "Role": "arn:aws:iam::1:role/r",
                    "Environment": {
                        "Variables": {
                            "DATABASE_URL": "postgres://…",
                            "REDIS_URL": "redis://…",
                            "LOG_LEVEL": "info"
                        }
                    },
                    "ReservedConcurrentExecutions": 50,
                    "TracingConfig": {"Mode": "Active"},
                    "DeadLetterConfig": {"TargetArn": "arn:aws:sqs:us-east-1:1:dlq"}
                }
            ]
        }"#;
        let resp: ListFunctionsResponse = serde_json::from_str(json).unwrap();
        let fn_ = &resp.functions[0];
        let env = fn_.environment.as_ref().expect("environment present");
        assert_eq!(env.var_count(), 3);
        assert_eq!(fn_.reserved_concurrent_executions, Some(50));
        let tracing = fn_.tracing_config.as_ref().expect("tracing present");
        assert_eq!(tracing.mode.as_deref(), Some("Active"));
        let dlc = fn_.dead_letter_config.as_ref().expect("dlc present");
        assert!(dlc.target_arn.as_deref().unwrap_or("").ends_with(":dlq"));
    }

    #[test]
    fn environment_with_resolver_error_parses() {
        let json = r#"
        {
            "Variables": {},
            "Error": {
                "ErrorCode": "AccessDeniedException",
                "Message": "The role does not have permission to decrypt the env"
            }
        }"#;
        let env: EnvironmentResponse = serde_json::from_str(json).unwrap();
        assert_eq!(env.var_count(), 0);
        let err = env.error.as_ref().expect("error present");
        assert_eq!(err.error_code.as_deref(), Some("AccessDeniedException"));
    }

    #[test]
    fn missing_runtime_does_not_break_parse() {
        // Some packaged functions (container images, deprecated)
        // may omit Runtime — must still parse.
        let json = r#"{
            "Functions": [
                {
                    "FunctionName": "img-fn",
                    "FunctionArn": "arn:aws:lambda:us-east-1:1:function:img-fn",
                    "Role": "arn:aws:iam::1:role/r",
                    "PackageType": "Image"
                }
            ]
        }"#;
        let resp: ListFunctionsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.functions[0].runtime.is_none());
    }
}
