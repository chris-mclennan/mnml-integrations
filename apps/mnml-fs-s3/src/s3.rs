//! Thin wrappers around `aws s3` and `aws s3api` for the operations
//! we need: list a prefix, head an object, download, upload, delete,
//! presign. All sync — called from worker threads via channels.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One row in a listing — either a common prefix (folder-ish) or a
/// concrete object (file).
#[derive(Debug, Clone)]
pub enum Entry {
    Prefix(PrefixEntry),
    Object(ObjectEntry),
}

#[derive(Debug, Clone)]
pub struct PrefixEntry {
    /// The prefix relative to the listing — `errors/` (not the full key).
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ObjectEntry {
    /// Object name relative to the listing — `build-log.txt`.
    pub name: String,
    /// Full key (with parent prefix prepended) — `2026/06/build-log.txt`.
    pub key: String,
    pub size: u64,
    /// ISO 8601 last-modified — we keep the raw string and slice
    /// for the date in the table (skip a chrono parse on the render path).
    pub last_modified: String,
    /// Storage class — `STANDARD`, `STANDARD_IA`, `GLACIER`, etc.
    /// Surfaced as an inline chip in v0.2.
    #[allow(dead_code)]
    pub storage_class: Option<String>,
}

impl Entry {
    #[allow(dead_code)]
    pub fn display_name(&self) -> &str {
        match self {
            Entry::Prefix(p) => &p.name,
            Entry::Object(o) => &o.name,
        }
    }

    #[allow(dead_code)]
    pub fn is_dir(&self) -> bool {
        matches!(self, Entry::Prefix(_))
    }
}

/// List one prefix level (`aws s3api list-objects-v2 --delimiter /
/// --prefix <prefix>`). The delimiter makes S3 return common
/// prefixes for the next level — the "folder-ish" rows in the UI.
pub fn list_prefix(bucket: &str, prefix: &str, region: Option<&str>) -> Result<Vec<Entry>> {
    let mut args: Vec<String> = vec![
        "s3api".into(),
        "list-objects-v2".into(),
        "--bucket".into(),
        bucket.into(),
        "--delimiter".into(),
        "/".into(),
        "--max-keys".into(),
        "1000".into(),
    ];
    if !prefix.is_empty() {
        args.push("--prefix".into());
        args.push(prefix.into());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let json = run_aws(&arg_refs, region)?;

    let response: ListObjectsResponse =
        serde_json::from_value(json).context("parse list-objects-v2 response")?;

    let mut out: Vec<Entry> = Vec::new();
    for p in response.common_prefixes.unwrap_or_default() {
        // CommonPrefixes contains the full path including parent
        // prefix; strip it to get a short display name.
        let stripped = p
            .prefix
            .strip_prefix(prefix)
            .unwrap_or(&p.prefix)
            .to_string();
        out.push(Entry::Prefix(PrefixEntry { name: stripped }));
    }
    for o in response.contents.unwrap_or_default() {
        // Objects can appear at the prefix itself (key == prefix) —
        // skip those rows in the listing.
        if o.key == prefix {
            continue;
        }
        let short = o.key.strip_prefix(prefix).unwrap_or(&o.key).to_string();
        out.push(Entry::Object(ObjectEntry {
            name: short,
            key: o.key,
            size: o.size.unwrap_or(0),
            last_modified: o.last_modified.unwrap_or_default(),
            storage_class: o.storage_class,
        }));
    }
    Ok(out)
}

/// Download a key to a local path via `aws s3 cp s3://bucket/key
/// <out>`. The destination directory is created if needed. Returns
/// the resolved destination path.
pub fn download(bucket: &str, key: &str, dest: &Path, region: Option<&str>) -> Result<PathBuf> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    let uri = format!("s3://{bucket}/{key}");
    let dest_s = dest.to_string_lossy().to_string();
    let arg_refs: Vec<&str> = vec!["s3", "cp", &uri, &dest_s];
    run_aws_void(&arg_refs, region)?;
    Ok(dest.to_path_buf())
}

/// Upload a local file to `s3://bucket/key` via `aws s3 cp`. Retained
/// as the small-file / diagnostic path — the interactive upload UI
/// (#1047 progress bar + #1048 multi-select) now runs through
/// `crate::upload::spawn_upload`, which pipes progress back on a
/// channel while the same `aws s3 cp` binary does the PUT + automatic
/// multipart under the hood.
#[allow(dead_code)]
pub fn upload(local: &Path, bucket: &str, key: &str, region: Option<&str>) -> Result<()> {
    let uri = format!("s3://{bucket}/{key}");
    let local_s = local.to_string_lossy().to_string();
    let arg_refs: Vec<&str> = vec!["s3", "cp", &local_s, &uri];
    run_aws_void(&arg_refs, region)?;
    Ok(())
}

/// Delete one key via `aws s3 rm s3://bucket/key`. Caller gates this
/// behind a confirmation prompt — there's no undo.
pub fn delete(bucket: &str, key: &str, region: Option<&str>) -> Result<()> {
    let uri = format!("s3://{bucket}/{key}");
    let arg_refs: Vec<&str> = vec!["s3", "rm", &uri];
    run_aws_void(&arg_refs, region)?;
    Ok(())
}

/// Generate a presigned URL via `aws s3 presign`. Default expires
/// in 5 minutes (300 sec) — short by design; for sharing further,
/// users can re-presign with `--expires-in`.
pub fn presign(bucket: &str, key: &str, region: Option<&str>) -> Result<String> {
    let uri = format!("s3://{bucket}/{key}");
    let arg_refs: Vec<&str> = vec!["s3", "presign", &uri, "--expires-in", "300"];
    run_aws_text(&arg_refs, region)
}

/// Returns the AWS console URL for a bucket / prefix — handy for
/// the `o` keybinding (open-in-browser).
pub fn console_url(bucket: &str, prefix: &str, region: Option<&str>) -> String {
    let region = region.unwrap_or("us-east-1");
    let prefix_part = if prefix.is_empty() {
        String::new()
    } else {
        format!("?prefix={}", urlencode(prefix))
    };
    format!("https://{region}.console.aws.amazon.com/s3/buckets/{bucket}{prefix_part}")
}

#[derive(Debug, Deserialize)]
struct ListObjectsResponse {
    #[serde(rename = "CommonPrefixes")]
    common_prefixes: Option<Vec<CommonPrefix>>,
    #[serde(rename = "Contents")]
    contents: Option<Vec<S3Object>>,
}

#[derive(Debug, Deserialize)]
struct CommonPrefix {
    #[serde(rename = "Prefix")]
    prefix: String,
}

#[derive(Debug, Deserialize)]
struct S3Object {
    #[serde(rename = "Key")]
    key: String,
    #[serde(rename = "Size")]
    size: Option<u64>,
    #[serde(rename = "LastModified")]
    last_modified: Option<String>,
    #[serde(rename = "StorageClass")]
    storage_class: Option<String>,
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

fn run_aws_void(args: &[&str], region: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("aws");
    if let Some(r) = region {
        cmd.arg("--region").arg(r);
    }
    cmd.args(args);
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
    Ok(())
}

fn run_aws_text(args: &[&str], region: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("aws");
    if let Some(r) = region {
        cmd.arg("--region").arg(r);
    }
    cmd.args(args);
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
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn urlencode(s: &str) -> String {
    // Minimal URL encoding — only the chars we actually see in S3
    // prefixes (slash + safe alphas pass through; spaces / non-ASCII
    // get percent-encoded). Avoids pulling in a urlencoding dep.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

/// Format a byte count as a short human-readable string —
/// `1.2 MB`, `45 KB`, etc. Used by the UI's size column.
pub fn fmt_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_formatting() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(1024), "1 KB");
        assert_eq!(fmt_size(1_500_000), "1.4 MB");
        assert_eq!(fmt_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn urlencode_handles_spaces_and_slashes() {
        assert_eq!(urlencode("foo/bar"), "foo/bar");
        assert_eq!(urlencode("foo bar"), "foo%20bar");
        assert_eq!(urlencode("hello-world_1.txt"), "hello-world_1.txt");
    }

    #[test]
    fn console_url_with_prefix() {
        let url = console_url("my-bucket", "logs/2026/", Some("us-west-2"));
        assert!(url.contains("us-west-2.console.aws.amazon.com"));
        assert!(url.contains("buckets/my-bucket"));
        assert!(url.contains("prefix=logs/2026/"));
    }

    #[test]
    fn console_url_without_prefix_omits_query() {
        let url = console_url("my-bucket", "", Some("us-east-1"));
        assert!(!url.contains('?'));
    }
}
