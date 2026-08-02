//! `aws ecr describe-repositories` / `describe-images` shell-outs +
//! structured response models. Pure CLI — no SDK dep.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageScanningConfiguration {
    #[serde(rename = "scanOnPush", default)]
    pub scan_on_push: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    #[serde(rename = "repositoryName")]
    pub name: String,
    #[serde(rename = "repositoryArn", default)]
    pub arn: String,
    #[serde(rename = "registryId", default)]
    pub registry_id: Option<String>,
    #[serde(rename = "repositoryUri", default)]
    pub uri: Option<String>,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<String>,
    #[serde(rename = "imageTagMutability", default)]
    pub tag_mutability: Option<String>,
    #[serde(rename = "imageScanningConfiguration", default)]
    pub scanning: Option<ImageScanningConfiguration>,
    #[serde(rename = "encryptionConfiguration", default)]
    pub encryption: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDetail {
    #[serde(rename = "repositoryName")]
    pub repository_name: String,
    #[serde(rename = "imageDigest", default)]
    pub digest: Option<String>,
    #[serde(rename = "imageTags", default)]
    pub tags: Vec<String>,
    #[serde(rename = "imageSizeInBytes", default)]
    pub size_bytes: Option<u64>,
    #[serde(rename = "imagePushedAt", default)]
    pub pushed_at: Option<String>,
    #[serde(rename = "imageManifestMediaType", default)]
    pub manifest_media_type: Option<String>,
    #[serde(rename = "artifactMediaType", default)]
    pub artifact_media_type: Option<String>,
    #[serde(rename = "imageScanFindingsSummary", default)]
    pub scan_summary: Option<ScanFindingsSummary>,
    #[serde(rename = "imageScanStatus", default)]
    pub scan_status: Option<ScanStatus>,
}

/// Subset of the `imageScanFindingsSummary` shape — the per-severity
/// vulnerability counts. AWS includes the keys `CRITICAL`, `HIGH`,
/// `MEDIUM`, `LOW`, `INFORMATIONAL`, `UNDEFINED`; missing keys mean
/// zero. Stored as a HashMap to forward-compat new severities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanFindingsSummary {
    #[serde(rename = "imageScanCompletedAt", default)]
    pub completed_at: Option<String>,
    #[serde(rename = "vulnerabilitySourceUpdatedAt", default)]
    pub vuln_source_updated_at: Option<String>,
    #[serde(rename = "findingSeverityCounts", default)]
    pub counts: std::collections::HashMap<String, u64>,
}

impl ScanFindingsSummary {
    pub fn critical(&self) -> u64 {
        self.counts.get("CRITICAL").copied().unwrap_or(0)
    }
    pub fn high(&self) -> u64 {
        self.counts.get("HIGH").copied().unwrap_or(0)
    }
    pub fn medium(&self) -> u64 {
        self.counts.get("MEDIUM").copied().unwrap_or(0)
    }
    pub fn low(&self) -> u64 {
        self.counts.get("LOW").copied().unwrap_or(0)
    }
    pub fn informational(&self) -> u64 {
        self.counts.get("INFORMATIONAL").copied().unwrap_or(0)
    }
    pub fn total(&self) -> u64 {
        self.critical() + self.high() + self.medium() + self.low() + self.informational()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanStatus {
    #[serde(rename = "status", default)]
    pub status: Option<String>,
    #[serde(rename = "description", default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DescribeRepositoriesResponse {
    #[serde(rename = "repositories")]
    repositories: Vec<Repository>,
    #[serde(rename = "nextToken", default)]
    next_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DescribeImagesResponse {
    #[serde(rename = "imageDetails")]
    image_details: Vec<ImageDetail>,
    #[serde(rename = "nextToken", default)]
    next_token: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Repository(Repository),
    Image(ImageDetail),
}

impl Item {
    pub fn primary_label(&self) -> &str {
        match self {
            Item::Repository(r) => &r.name,
            Item::Image(i) => i
                .tags
                .first()
                .map(|s| s.as_str())
                .unwrap_or_else(|| short_digest(i.digest.as_deref().unwrap_or("(untagged)"))),
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Repository(r) => {
                let mutability = r
                    .tag_mutability
                    .as_deref()
                    .unwrap_or("?")
                    .to_ascii_lowercase();
                let scan = match r.scanning.as_ref().and_then(|s| s.scan_on_push) {
                    Some(true) => "scan-on-push",
                    Some(false) => "no-scan",
                    None => "?",
                };
                format!("{mutability} · {scan}")
            }
            Item::Image(i) => {
                let size = i.size_bytes.map(fmt_bytes).unwrap_or_else(|| "—".into());
                let pushed = i
                    .pushed_at
                    .as_deref()
                    .map(short_timestamp)
                    .unwrap_or_else(|| "—".into());
                let extra_tags = i.tags.len().saturating_sub(1);
                // ⚠ Critical/high vulnerability chip — surfaces images
                // with scan findings. Critical wins over high; we only
                // show one chip to keep the row scannable.
                let scan_chip = match i.scan_summary.as_ref() {
                    Some(s) if s.critical() > 0 => {
                        format!(" · ⚠ {}c", s.critical())
                    }
                    Some(s) if s.high() > 0 => {
                        format!(" · ⚠ {}h", s.high())
                    }
                    _ => String::new(),
                };
                if extra_tags > 0 {
                    format!("{size} · {pushed} · +{extra_tags} tag(s){scan_chip}")
                } else {
                    format!("{size} · {pushed}{scan_chip}")
                }
            }
        }
    }
}

pub fn describe_repositories(region: Option<&str>) -> Result<Vec<Repository>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut cmd = Command::new("aws");
        cmd.args(["ecr", "describe-repositories", "--output", "json"]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(t) = &token {
            cmd.args(["--next-token", t]);
        }
        let output = cmd
            .output()
            .with_context(|| "spawn `aws ecr describe-repositories`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws ecr describe-repositories failed: {}",
                stderr.trim()
            ));
        }
        let resp: DescribeRepositoriesResponse = serde_json::from_slice(&output.stdout)
            .with_context(|| "parse describe-repositories JSON")?;
        all.extend(resp.repositories);
        match resp.next_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    all.sort_by_key(|r| r.name.to_lowercase());
    Ok(all)
}

pub fn describe_images(repository: &str, region: Option<&str>) -> Result<Vec<ImageDetail>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut cmd = Command::new("aws");
        cmd.args([
            "ecr",
            "describe-images",
            "--repository-name",
            repository,
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
            .with_context(|| format!("spawn `aws ecr describe-images` for {repository}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws ecr describe-images failed for {repository}: {}",
                stderr.trim()
            ));
        }
        let resp: DescribeImagesResponse =
            serde_json::from_slice(&output.stdout).with_context(|| "parse describe-images JSON")?;
        all.extend(resp.image_details);
        match resp.next_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    // Sort by pushed_at desc — newest at the top, matching docker UI.
    all.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));
    Ok(all)
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

/// Trim a `2026-06-06T18:30:00.123Z` timestamp down to `2026-06-06 18:30`
/// (no seconds — image push dates rarely care about precision).
pub fn short_timestamp(ts: &str) -> String {
    if ts.len() >= 16 {
        ts[..16].replace('T', " ")
    } else {
        ts.to_string()
    }
}

/// Strip the `sha256:` prefix from a digest and keep only the first 12
/// chars for a short display form (`sha256:abc123def456…` → `abc123def456`).
pub fn short_digest(digest: &str) -> &str {
    let stripped = digest.strip_prefix("sha256:").unwrap_or(digest);
    if stripped.len() > 12 {
        &stripped[..12]
    } else {
        stripped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_describe_repositories_response() {
        let json = r#"{
            "repositories": [
                {
                    "repositoryName": "api",
                    "repositoryArn": "arn:aws:ecr:us-east-1:1:repository/api",
                    "registryId": "111111111111",
                    "repositoryUri": "111111111111.dkr.ecr.us-east-1.amazonaws.com/api",
                    "createdAt": "2024-01-01T00:00:00.000Z",
                    "imageTagMutability": "MUTABLE",
                    "imageScanningConfiguration": {"scanOnPush": true}
                }
            ]
        }"#;
        let resp: DescribeRepositoriesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.repositories.len(), 1);
        let r = &resp.repositories[0];
        assert_eq!(r.name, "api");
        assert_eq!(r.scanning.as_ref().and_then(|s| s.scan_on_push), Some(true));
    }

    #[test]
    fn parses_describe_images_response() {
        let json = r#"{
            "imageDetails": [
                {
                    "repositoryName": "api",
                    "imageDigest": "sha256:abc1234567890def1234567890abcdef1234567890abcdef1234567890abcdef",
                    "imageTags": ["v1.2.3", "latest"],
                    "imageSizeInBytes": 50331648,
                    "imagePushedAt": "2026-06-06T18:30:00.000Z"
                }
            ]
        }"#;
        let resp: DescribeImagesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.image_details.len(), 1);
        let img = &resp.image_details[0];
        assert_eq!(img.tags, vec!["v1.2.3", "latest"]);
        assert_eq!(img.size_bytes, Some(50_331_648));
    }

    #[test]
    fn fmt_bytes_picks_right_unit() {
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(50_331_648), "48.0 MB");
    }

    #[test]
    fn short_timestamp_drops_seconds() {
        assert_eq!(
            short_timestamp("2026-06-06T18:30:00.123Z"),
            "2026-06-06 18:30"
        );
    }

    #[test]
    fn short_digest_strips_sha_prefix_and_truncates() {
        assert_eq!(
            short_digest("sha256:abc1234567890def1234567890abcdef"),
            "abc123456789"
        );
        assert_eq!(short_digest("noprefix"), "noprefix");
    }

    #[test]
    fn parses_scan_findings_summary() {
        let json = r#"{
            "imageDetails": [
                {
                    "repositoryName": "api",
                    "imageDigest": "sha256:abc",
                    "imageTags": ["v1.2.3"],
                    "imageScanFindingsSummary": {
                        "imageScanCompletedAt": "2026-06-06T18:30:00.000Z",
                        "findingSeverityCounts": {
                            "CRITICAL": 2,
                            "HIGH": 5,
                            "MEDIUM": 10,
                            "LOW": 8,
                            "INFORMATIONAL": 3
                        }
                    },
                    "imageScanStatus": {
                        "status": "COMPLETE",
                        "description": "The scan was completed successfully."
                    }
                }
            ]
        }"#;
        let resp: DescribeImagesResponse = serde_json::from_str(json).unwrap();
        let img = &resp.image_details[0];
        let scan = img.scan_summary.as_ref().expect("scan summary present");
        assert_eq!(scan.critical(), 2);
        assert_eq!(scan.high(), 5);
        assert_eq!(scan.medium(), 10);
        assert_eq!(scan.low(), 8);
        assert_eq!(scan.informational(), 3);
        assert_eq!(scan.total(), 28);
        let status = img.scan_status.as_ref().expect("scan status present");
        assert_eq!(status.status.as_deref(), Some("COMPLETE"));
    }

    #[test]
    fn scan_summary_missing_severity_counts_as_zero() {
        let json = r#"{
            "findingSeverityCounts": {"CRITICAL": 1}
        }"#;
        let scan: ScanFindingsSummary = serde_json::from_str(json).unwrap();
        assert_eq!(scan.critical(), 1);
        assert_eq!(scan.high(), 0);
        assert_eq!(scan.medium(), 0);
    }

    #[test]
    fn item_secondary_label_surfaces_critical_chip() {
        let img = ImageDetail {
            repository_name: "api".into(),
            digest: None,
            tags: vec!["v1".into()],
            size_bytes: Some(1024 * 1024),
            pushed_at: None,
            manifest_media_type: None,
            artifact_media_type: None,
            scan_summary: Some(ScanFindingsSummary {
                completed_at: None,
                vuln_source_updated_at: None,
                counts: [("CRITICAL".to_string(), 3)].into_iter().collect(),
            }),
            scan_status: None,
        };
        let label = Item::Image(img).secondary_label();
        assert!(label.contains("⚠ 3c"));
    }

    #[test]
    fn item_secondary_label_falls_back_to_high_chip() {
        let img = ImageDetail {
            repository_name: "api".into(),
            digest: None,
            tags: vec!["v1".into()],
            size_bytes: None,
            pushed_at: None,
            manifest_media_type: None,
            artifact_media_type: None,
            scan_summary: Some(ScanFindingsSummary {
                completed_at: None,
                vuln_source_updated_at: None,
                counts: [("HIGH".to_string(), 7)].into_iter().collect(),
            }),
            scan_status: None,
        };
        let label = Item::Image(img).secondary_label();
        assert!(label.contains("⚠ 7h"));
        assert!(!label.contains("c"));
    }
}
