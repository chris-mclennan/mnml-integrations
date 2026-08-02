//! `aws sqs list-queues` / `get-queue-attributes` shell-outs +
//! structured response models. Pure CLI — no SDK dep.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct ListQueuesResponse {
    #[serde(rename = "QueueUrls", default)]
    queue_urls: Vec<String>,
    #[serde(rename = "NextToken", default)]
    next_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetQueueAttributesResponse {
    #[serde(rename = "Attributes", default)]
    attributes: HashMap<String, String>,
}

/// Lazily-loaded attributes for a focused queue. The list view shows
/// just the URL + a few derived stats; the detail panel renders the
/// full attribute map when present.
#[derive(Debug, Clone, Default)]
pub struct QueueAttributes {
    pub raw: HashMap<String, String>,
}

impl QueueAttributes {
    pub fn from_map(raw: HashMap<String, String>) -> Self {
        Self { raw }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key).map(|s| s.as_str())
    }

    pub fn approximate_messages(&self) -> Option<u64> {
        self.get("ApproximateNumberOfMessages")?.parse().ok()
    }

    pub fn approximate_messages_not_visible(&self) -> Option<u64> {
        self.get("ApproximateNumberOfMessagesNotVisible")?
            .parse()
            .ok()
    }

    pub fn approximate_messages_delayed(&self) -> Option<u64> {
        self.get("ApproximateNumberOfMessagesDelayed")?.parse().ok()
    }

    pub fn visibility_timeout(&self) -> Option<u64> {
        self.get("VisibilityTimeout")?.parse().ok()
    }

    pub fn message_retention_period(&self) -> Option<u64> {
        self.get("MessageRetentionPeriod")?.parse().ok()
    }

    pub fn delay_seconds(&self) -> Option<u64> {
        self.get("DelaySeconds")?.parse().ok()
    }

    pub fn maximum_message_size(&self) -> Option<u64> {
        self.get("MaximumMessageSize")?.parse().ok()
    }

    pub fn receive_message_wait_time_seconds(&self) -> Option<u64> {
        self.get("ReceiveMessageWaitTimeSeconds")?.parse().ok()
    }

    pub fn arn(&self) -> Option<&str> {
        self.get("QueueArn")
    }

    pub fn redrive_policy(&self) -> Option<&str> {
        self.get("RedrivePolicy")
    }

    /// `true` if this queue is the DLQ for at least one other queue
    /// (determined later by the parent app correlating RedrivePolicy
    /// fields). For now we just expose the redrive policy itself —
    /// the DLQ-marker correlation is a v0.2 cross-queue analysis.
    pub fn has_dlq(&self) -> bool {
        self.redrive_policy().is_some()
    }
}

/// Extract the queue name from a URL: the final path segment.
/// `https://sqs.us-east-1.amazonaws.com/1/my-queue` → `my-queue`.
pub fn queue_name_from_url(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[derive(Debug, Clone)]
pub struct Queue {
    pub url: String,
    pub attributes: Option<QueueAttributes>,
    /// Names of queues that have this queue *as their DLQ target*.
    /// Populated by `App::recompute_redrive_sources()` after the user
    /// has loaded all queues' attributes (the `A` action). Empty when
    /// not yet computed or when no queue references this one as a DLQ.
    pub redrive_sources: Vec<String>,
}

impl Queue {
    pub fn name(&self) -> &str {
        queue_name_from_url(&self.url)
    }

    pub fn primary_label(&self) -> String {
        self.name().to_string()
    }

    pub fn secondary_label(&self) -> String {
        let Some(attrs) = &self.attributes else {
            return "(attrs not loaded)".to_string();
        };
        let visible = attrs.approximate_messages().unwrap_or(0);
        let in_flight = attrs.approximate_messages_not_visible().unwrap_or(0);
        let delayed = attrs.approximate_messages_delayed().unwrap_or(0);
        // Two distinct DLQ chips:
        //   ↓ DLQ — this queue *has* a DLQ configured for itself
        //   ↑ DLQ — this queue *is* a DLQ for N other queues
        // Both can apply simultaneously (rare but valid: a queue with
        // its own DLQ, which is itself a DLQ target — like a chain).
        let down_chip = if attrs.has_dlq() { " · ↓ DLQ" } else { "" };
        let up_chip = if !self.redrive_sources.is_empty() {
            if self.redrive_sources.len() == 1 {
                " · ↑ DLQ".to_string()
            } else {
                format!(" · ↑ DLQ x{}", self.redrive_sources.len())
            }
        } else {
            String::new()
        };
        let fifo_chip = if self.is_fifo() { " · FIFO" } else { "" };
        if delayed > 0 {
            format!(
                "{visible} msg · {in_flight} in-flight · {delayed} delayed{fifo_chip}{down_chip}{up_chip}"
            )
        } else {
            format!("{visible} msg · {in_flight} in-flight{fifo_chip}{down_chip}{up_chip}")
        }
    }

    pub fn is_fifo(&self) -> bool {
        self.url.ends_with(".fifo")
    }
}

/// Extract `deadLetterTargetArn` from a RedrivePolicy JSON blob (the
/// raw string SQS returns). Returns the ARN, or None if the policy is
/// malformed / missing the field. Independent of the rest of the policy
/// (we don't care about maxReceiveCount for correlation).
pub fn dlq_target_arn(redrive_policy: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(redrive_policy).ok()?;
    v.get("deadLetterTargetArn")?
        .as_str()
        .map(|s| s.to_string())
}

/// List every queue in the region. Pass `prefix=Some(...)` to scope to
/// queues whose name starts with that string.
pub fn list_queues(prefix: Option<&str>, region: Option<&str>) -> Result<Vec<String>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut cmd = Command::new("aws");
        cmd.args([
            "sqs",
            "list-queues",
            "--max-results",
            "1000",
            "--output",
            "json",
        ]);
        if let Some(p) = prefix {
            cmd.args(["--queue-name-prefix", p]);
        }
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(t) = &token {
            cmd.args(["--next-token", t]);
        }
        let output = cmd
            .output()
            .with_context(|| "spawn `aws sqs list-queues`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("aws sqs list-queues failed: {}", stderr.trim()));
        }
        let resp: ListQueuesResponse =
            serde_json::from_slice(&output.stdout).with_context(|| "parse list-queues JSON")?;
        all.extend(resp.queue_urls);
        match resp.next_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    all.sort_by_key(|u| queue_name_from_url(u).to_lowercase());
    Ok(all)
}

/// `get-queue-attributes --attribute-names All` for one queue.
pub fn get_attributes(url: &str, region: Option<&str>) -> Result<QueueAttributes> {
    let mut cmd = Command::new("aws");
    cmd.args([
        "sqs",
        "get-queue-attributes",
        "--queue-url",
        url,
        "--attribute-names",
        "All",
        "--output",
        "json",
    ]);
    if let Some(r) = region {
        cmd.args(["--region", r]);
    }
    let output = cmd
        .output()
        .with_context(|| format!("spawn `aws sqs get-queue-attributes` for {url}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "aws sqs get-queue-attributes failed for {url}: {}",
            stderr.trim()
        ));
    }
    let resp: GetQueueAttributesResponse = serde_json::from_slice(&output.stdout)
        .with_context(|| "parse get-queue-attributes JSON")?;
    Ok(QueueAttributes::from_map(resp.attributes))
}

/// Format a Unix-epoch-seconds value as a YYYY-MM-DD HH:MM string.
pub fn fmt_epoch_secs(s: &str) -> Option<String> {
    let n: i64 = s.parse().ok()?;
    use chrono::DateTime;
    DateTime::from_timestamp(n, 0).map(|d| d.format("%Y-%m-%d %H:%M").to_string())
}

/// Format a seconds value as a short human-readable duration:
/// `60` → `1m`, `345600` → `4d`, `1209600` → `14d`.
pub fn fmt_duration_secs(s: u64) -> String {
    if s >= 86400 {
        let d = s / 86400;
        let h = (s % 86400) / 3600;
        if h > 0 {
            format!("{d}d {h}h")
        } else {
            format!("{d}d")
        }
    } else if s >= 3600 {
        let h = s / 3600;
        let m = (s % 3600) / 60;
        if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else if s >= 60 {
        let m = s / 60;
        let r = s % 60;
        if r > 0 {
            format!("{m}m {r}s")
        } else {
            format!("{m}m")
        }
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_name_extracted_from_url() {
        assert_eq!(
            queue_name_from_url("https://sqs.us-east-1.amazonaws.com/123456789012/my-queue"),
            "my-queue"
        );
        assert_eq!(
            queue_name_from_url("https://sqs.us-east-1.amazonaws.com/123456789012/my-queue.fifo"),
            "my-queue.fifo"
        );
    }

    #[test]
    fn parses_list_queues_response() {
        let json = r#"{
            "QueueUrls": [
                "https://sqs.us-east-1.amazonaws.com/1/queue-a",
                "https://sqs.us-east-1.amazonaws.com/1/queue-b"
            ]
        }"#;
        let resp: ListQueuesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.queue_urls.len(), 2);
    }

    #[test]
    fn parses_get_queue_attributes_response() {
        let json = r#"{
            "Attributes": {
                "ApproximateNumberOfMessages": "42",
                "ApproximateNumberOfMessagesNotVisible": "3",
                "VisibilityTimeout": "30",
                "MessageRetentionPeriod": "1209600",
                "QueueArn": "arn:aws:sqs:us-east-1:1:my-queue",
                "RedrivePolicy": "{\"deadLetterTargetArn\":\"arn:…\",\"maxReceiveCount\":3}"
            }
        }"#;
        let resp: GetQueueAttributesResponse = serde_json::from_str(json).unwrap();
        let attrs = QueueAttributes::from_map(resp.attributes);
        assert_eq!(attrs.approximate_messages(), Some(42));
        assert_eq!(attrs.approximate_messages_not_visible(), Some(3));
        assert_eq!(attrs.visibility_timeout(), Some(30));
        assert!(attrs.has_dlq());
    }

    #[test]
    fn fifo_detected_from_url_suffix() {
        let q = Queue {
            url: "https://sqs.us-east-1.amazonaws.com/1/x.fifo".to_string(),
            attributes: None,
            redrive_sources: vec![],
        };
        assert!(q.is_fifo());
        let non = Queue {
            url: "https://sqs.us-east-1.amazonaws.com/1/x".to_string(),
            attributes: None,
            redrive_sources: vec![],
        };
        assert!(!non.is_fifo());
    }

    #[test]
    fn dlq_target_arn_extracted_from_redrive_policy() {
        let policy = r#"{"deadLetterTargetArn":"arn:aws:sqs:us-east-1:1:dlq","maxReceiveCount":3}"#;
        assert_eq!(
            dlq_target_arn(policy),
            Some("arn:aws:sqs:us-east-1:1:dlq".to_string())
        );
        assert!(dlq_target_arn("not json").is_none());
        assert!(dlq_target_arn(r#"{"maxReceiveCount":3}"#).is_none());
    }

    #[test]
    fn redrive_sources_chip_singular_vs_plural() {
        let attrs = QueueAttributes::from_map(HashMap::from([(
            "ApproximateNumberOfMessages".to_string(),
            "0".to_string(),
        )]));
        let mut q = Queue {
            url: "https://sqs.us-east-1.amazonaws.com/1/dlq".to_string(),
            attributes: Some(attrs.clone()),
            redrive_sources: vec!["source-a".to_string()],
        };
        assert!(q.secondary_label().contains("↑ DLQ"));
        assert!(!q.secondary_label().contains("x"));
        q.redrive_sources = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert!(q.secondary_label().contains("↑ DLQ x3"));
    }

    #[test]
    fn fmt_duration_secs_humanizes() {
        assert_eq!(fmt_duration_secs(30), "30s");
        assert_eq!(fmt_duration_secs(60), "1m");
        assert_eq!(fmt_duration_secs(90), "1m 30s");
        assert_eq!(fmt_duration_secs(3600), "1h");
        assert_eq!(fmt_duration_secs(3660), "1h 1m");
        assert_eq!(fmt_duration_secs(86400), "1d");
        assert_eq!(fmt_duration_secs(345600), "4d");
        assert_eq!(fmt_duration_secs(1209600), "14d");
    }

    #[test]
    fn secondary_label_no_attrs_says_so() {
        let q = Queue {
            url: "https://sqs.us-east-1.amazonaws.com/1/x".to_string(),
            attributes: None,
            redrive_sources: vec![],
        };
        assert!(q.secondary_label().contains("attrs not loaded"));
    }

    #[test]
    fn fmt_epoch_secs_formats_known_value() {
        let out = fmt_epoch_secs("1704067200").unwrap(); // 2024-01-01
        assert!(out.starts_with("2024-01-01"));
    }
}
