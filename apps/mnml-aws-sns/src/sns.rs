//! `aws sns list-topics` / `get-topic-attributes` / `list-subscriptions-by-topic`
//! shell-outs + structured response models. Pure CLI — no SDK dep.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct ListTopicsResponse {
    #[serde(rename = "Topics", default)]
    topics: Vec<TopicArnEntry>,
    #[serde(rename = "NextToken", default)]
    next_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TopicArnEntry {
    #[serde(rename = "TopicArn")]
    topic_arn: String,
}

#[derive(Debug, Deserialize)]
struct GetTopicAttributesResponse {
    #[serde(rename = "Attributes", default)]
    attributes: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ListSubscriptionsResponse {
    #[serde(rename = "Subscriptions", default)]
    subscriptions: Vec<Subscription>,
    #[serde(rename = "NextToken", default)]
    next_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Subscription {
    #[serde(rename = "SubscriptionArn", default)]
    pub arn: String,
    #[serde(rename = "Owner", default)]
    pub owner: Option<String>,
    #[serde(rename = "Protocol", default)]
    pub protocol: Option<String>,
    #[serde(rename = "Endpoint", default)]
    pub endpoint: Option<String>,
    #[serde(rename = "TopicArn", default)]
    pub topic_arn: Option<String>,
}

impl Subscription {
    /// SNS returns the literal string `"PendingConfirmation"` as the
    /// ARN for subscriptions that haven't been confirmed yet — they
    /// don't have a real ARN until then. Use this to color-code.
    pub fn is_pending_confirmation(&self) -> bool {
        self.arn == "PendingConfirmation" || self.arn.is_empty()
    }

    pub fn endpoint_short(&self) -> &str {
        match self.endpoint.as_deref() {
            Some(e) => {
                // For long ARNs (Lambda / SQS), trim to last segment.
                if e.starts_with("arn:") {
                    e.rsplit(':').next().unwrap_or(e)
                } else {
                    e
                }
            }
            None => "—",
        }
    }
}

/// Lazily-loaded attributes for a focused topic.
#[derive(Debug, Clone, Default)]
pub struct TopicAttributes {
    pub raw: HashMap<String, String>,
}

impl TopicAttributes {
    pub fn from_map(raw: HashMap<String, String>) -> Self {
        Self { raw }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key).map(|s| s.as_str())
    }

    pub fn display_name(&self) -> Option<&str> {
        self.get("DisplayName")
    }

    pub fn owner(&self) -> Option<&str> {
        self.get("Owner")
    }

    pub fn subscriptions_confirmed(&self) -> Option<u64> {
        self.get("SubscriptionsConfirmed")?.parse().ok()
    }

    pub fn subscriptions_pending(&self) -> Option<u64> {
        self.get("SubscriptionsPending")?.parse().ok()
    }

    pub fn subscriptions_deleted(&self) -> Option<u64> {
        self.get("SubscriptionsDeleted")?.parse().ok()
    }

    pub fn kms_master_key_id(&self) -> Option<&str> {
        self.get("KmsMasterKeyId")
    }

    pub fn fifo_topic(&self) -> bool {
        self.get("FifoTopic") == Some("true")
    }

    pub fn signature_version(&self) -> Option<&str> {
        self.get("SignatureVersion")
    }

    pub fn delivery_policy(&self) -> Option<&str> {
        self.get("DeliveryPolicy")
    }
}

/// Extract the short topic name (the last segment of the ARN).
/// `arn:aws:sns:us-east-1:1:my-topic` → `my-topic`.
pub fn topic_name_from_arn(arn: &str) -> &str {
    arn.rsplit(':').next().unwrap_or(arn)
}

#[derive(Debug, Clone)]
pub struct Topic {
    pub arn: String,
    pub attributes: Option<TopicAttributes>,
}

impl Topic {
    pub fn name(&self) -> &str {
        topic_name_from_arn(&self.arn)
    }

    pub fn is_fifo(&self) -> bool {
        self.arn.ends_with(".fifo")
            || self
                .attributes
                .as_ref()
                .map(|a| a.fifo_topic())
                .unwrap_or(false)
    }

    pub fn secondary_label(&self) -> String {
        let Some(attrs) = &self.attributes else {
            return "(attrs not loaded)".to_string();
        };
        let confirmed = attrs.subscriptions_confirmed().unwrap_or(0);
        let pending = attrs.subscriptions_pending().unwrap_or(0);
        let fifo_chip = if self.is_fifo() { " · FIFO" } else { "" };
        if pending > 0 {
            format!("{confirmed} sub · {pending} pending{fifo_chip}")
        } else {
            format!("{confirmed} sub{fifo_chip}")
        }
    }
}

/// List every topic in the region. Optionally filter by short-name
/// prefix (applied client-side because SNS's `list-topics` doesn't
/// support a name filter).
pub fn list_topics(prefix: Option<&str>, region: Option<&str>) -> Result<Vec<String>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut cmd = Command::new("aws");
        cmd.args(["sns", "list-topics", "--output", "json"]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(t) = &token {
            cmd.args(["--next-token", t]);
        }
        let output = cmd
            .output()
            .with_context(|| "spawn `aws sns list-topics`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("aws sns list-topics failed: {}", stderr.trim()));
        }
        let resp: ListTopicsResponse =
            serde_json::from_slice(&output.stdout).with_context(|| "parse list-topics JSON")?;
        for entry in resp.topics {
            all.push(entry.topic_arn);
        }
        match resp.next_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    if let Some(p) = prefix {
        all.retain(|arn| topic_name_from_arn(arn).starts_with(p));
    }
    all.sort_by_key(|a| topic_name_from_arn(a).to_lowercase());
    Ok(all)
}

/// `get-topic-attributes` for a single topic.
pub fn get_topic_attributes(arn: &str, region: Option<&str>) -> Result<TopicAttributes> {
    let mut cmd = Command::new("aws");
    cmd.args([
        "sns",
        "get-topic-attributes",
        "--topic-arn",
        arn,
        "--output",
        "json",
    ]);
    if let Some(r) = region {
        cmd.args(["--region", r]);
    }
    let output = cmd
        .output()
        .with_context(|| format!("spawn `aws sns get-topic-attributes` for {arn}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "aws sns get-topic-attributes failed for {arn}: {}",
            stderr.trim()
        ));
    }
    let resp: GetTopicAttributesResponse = serde_json::from_slice(&output.stdout)
        .with_context(|| "parse get-topic-attributes JSON")?;
    Ok(TopicAttributes::from_map(resp.attributes))
}

/// `list-subscriptions-by-topic` for a single topic. Paginates.
pub fn list_subscriptions_by_topic(arn: &str, region: Option<&str>) -> Result<Vec<Subscription>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut cmd = Command::new("aws");
        cmd.args([
            "sns",
            "list-subscriptions-by-topic",
            "--topic-arn",
            arn,
            "--output",
            "json",
        ]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(t) = &token {
            cmd.args(["--next-token", t]);
        }
        let output = cmd
            .output()
            .with_context(|| format!("spawn `aws sns list-subscriptions-by-topic` for {arn}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws sns list-subscriptions-by-topic failed for {arn}: {}",
                stderr.trim()
            ));
        }
        let resp: ListSubscriptionsResponse = serde_json::from_slice(&output.stdout)
            .with_context(|| "parse list-subscriptions-by-topic JSON")?;
        all.extend(resp.subscriptions);
        match resp.next_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    // Stable order: protocol then endpoint.
    all.sort_by(|a, b| {
        a.protocol
            .cmp(&b.protocol)
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_name_extracted_from_arn() {
        assert_eq!(
            topic_name_from_arn("arn:aws:sns:us-east-1:111111111111:my-topic"),
            "my-topic"
        );
        assert_eq!(
            topic_name_from_arn("arn:aws:sns:us-east-1:111111111111:orders.fifo"),
            "orders.fifo"
        );
    }

    #[test]
    fn parses_list_topics_response() {
        let json = r#"{
            "Topics": [
                {"TopicArn": "arn:aws:sns:us-east-1:1:a"},
                {"TopicArn": "arn:aws:sns:us-east-1:1:b"}
            ]
        }"#;
        let resp: ListTopicsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.topics.len(), 2);
        assert_eq!(resp.topics[0].topic_arn, "arn:aws:sns:us-east-1:1:a");
    }

    #[test]
    fn parses_get_topic_attributes_response() {
        let json = r#"{
            "Attributes": {
                "DisplayName": "Orders",
                "Owner": "111111111111",
                "SubscriptionsConfirmed": "3",
                "SubscriptionsPending": "1",
                "SubscriptionsDeleted": "0",
                "FifoTopic": "false",
                "SignatureVersion": "1"
            }
        }"#;
        let resp: GetTopicAttributesResponse = serde_json::from_str(json).unwrap();
        let attrs = TopicAttributes::from_map(resp.attributes);
        assert_eq!(attrs.display_name(), Some("Orders"));
        assert_eq!(attrs.subscriptions_confirmed(), Some(3));
        assert_eq!(attrs.subscriptions_pending(), Some(1));
        assert!(!attrs.fifo_topic());
    }

    #[test]
    fn parses_list_subscriptions_response() {
        let json = r#"{
            "Subscriptions": [
                {
                    "SubscriptionArn": "arn:aws:sns:us-east-1:1:topic:subid-1",
                    "Owner": "111111111111",
                    "Protocol": "sqs",
                    "Endpoint": "arn:aws:sqs:us-east-1:1:my-queue",
                    "TopicArn": "arn:aws:sns:us-east-1:1:topic"
                },
                {
                    "SubscriptionArn": "PendingConfirmation",
                    "Owner": "111111111111",
                    "Protocol": "email",
                    "Endpoint": "ops@example.com",
                    "TopicArn": "arn:aws:sns:us-east-1:1:topic"
                }
            ]
        }"#;
        let resp: ListSubscriptionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.subscriptions.len(), 2);
        assert!(!resp.subscriptions[0].is_pending_confirmation());
        assert!(resp.subscriptions[1].is_pending_confirmation());
    }

    #[test]
    fn subscription_endpoint_short_trims_arn_to_tail() {
        let s = Subscription {
            arn: "arn:aws:sns:us-east-1:1:topic:subid".into(),
            owner: None,
            protocol: Some("sqs".into()),
            endpoint: Some("arn:aws:sqs:us-east-1:1:my-queue".into()),
            topic_arn: None,
        };
        assert_eq!(s.endpoint_short(), "my-queue");
    }

    #[test]
    fn fifo_detected_from_arn_suffix() {
        let t = Topic {
            arn: "arn:aws:sns:us-east-1:1:orders.fifo".into(),
            attributes: None,
        };
        assert!(t.is_fifo());
        let non = Topic {
            arn: "arn:aws:sns:us-east-1:1:orders".into(),
            attributes: None,
        };
        assert!(!non.is_fifo());
    }

    #[test]
    fn secondary_label_loading_state() {
        let t = Topic {
            arn: "arn:aws:sns:us-east-1:1:x".into(),
            attributes: None,
        };
        assert!(t.secondary_label().contains("attrs not loaded"));
    }
}
