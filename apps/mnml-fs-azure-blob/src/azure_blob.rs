//! Thin wrappers around the `az storage` CLI for the operations
//! we need: list storage accounts, list containers in an account,
//! list blobs (optionally under a prefix), show a blob's properties,
//! download a blob. All sync — called from worker threads via channels.
//!
//! Auth — every call defers to the `az` CLI's own credential chain
//! (`az login` interactive, env vars `AZURE_CLIENT_ID` /
//! `AZURE_TENANT_ID` / `AZURE_CLIENT_SECRET`, managed identity, etc.).
//! The sibling doesn't manage tokens.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One row in a tab — a storage account, a container, a blob
/// "folder" prefix (when the listing is delimited), or a concrete blob.
#[derive(Debug, Clone)]
pub enum Entry {
    Account(AccountEntry),
    Container(ContainerEntry),
    Prefix(PrefixEntry),
    Blob(BlobEntry),
}

#[derive(Debug, Clone)]
pub struct AccountEntry {
    pub name: String,
    pub location: String,
    pub kind: String,
    /// Reserved for v0.2 — surface on the right-pane detail.
    #[allow(dead_code)]
    pub resource_group: String,
    /// Reserved for v0.2 — surface on the right-pane detail.
    #[allow(dead_code)]
    pub blob_endpoint: Option<String>,
    /// Reserved for v0.2 — surface on the right-pane detail.
    #[allow(dead_code)]
    pub sku: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContainerEntry {
    pub name: String,
    pub last_modified: String,
    pub public_access: Option<String>,
    /// Reserved for v0.2 — surface on the right-pane detail.
    #[allow(dead_code)]
    pub lease_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PrefixEntry {
    /// The prefix relative to the listing — `errors/` (not the full name).
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct BlobEntry {
    /// Display name relative to the listing — `build-log.txt`.
    pub name: String,
    /// Full blob name (with parent prefix prepended) — `2026/06/build-log.txt`.
    pub full_name: String,
    pub size: u64,
    pub last_modified: String,
    /// Reserved for v0.2 — surface on the right-pane detail.
    #[allow(dead_code)]
    pub content_type: Option<String>,
    /// Blob access tier — `Hot`, `Cool`, `Archive`.
    /// Reserved for v0.2 — surface as an inline chip.
    #[allow(dead_code)]
    pub blob_tier: Option<String>,
}

impl Entry {
    #[allow(dead_code)]
    pub fn display_name(&self) -> &str {
        match self {
            Entry::Account(a) => &a.name,
            Entry::Container(c) => &c.name,
            Entry::Prefix(p) => &p.name,
            Entry::Blob(b) => &b.name,
        }
    }

    #[allow(dead_code)]
    pub fn is_drillable(&self) -> bool {
        matches!(
            self,
            Entry::Account(_) | Entry::Container(_) | Entry::Prefix(_)
        )
    }
}

/// List Azure storage accounts visible to the current `az` login.
/// `az storage account list --output json`.
pub fn list_accounts() -> Result<Vec<Entry>> {
    let args = ["storage", "account", "list"];
    let json = run_az(&args)?;
    let raw: Vec<AccountRaw> =
        serde_json::from_value(json).context("parse storage account list response")?;
    Ok(raw
        .into_iter()
        .map(|a| {
            Entry::Account(AccountEntry {
                name: a.name,
                location: a.location.unwrap_or_default(),
                kind: a.kind.unwrap_or_default(),
                resource_group: a.resource_group.unwrap_or_default(),
                blob_endpoint: a.primary_endpoints.and_then(|e| e.blob),
                sku: a.sku.map(|s| s.name),
            })
        })
        .collect())
}

/// List containers in a storage account.
/// `az storage container list --account-name <name> --output json`.
///
/// Authenticates with AAD by default (`--auth-mode login`); falls
/// back to the account's connection string if Azure refuses AAD.
pub fn list_containers(account: &str) -> Result<Vec<Entry>> {
    let args = [
        "storage",
        "container",
        "list",
        "--account-name",
        account,
        "--auth-mode",
        "login",
    ];
    let json = run_az(&args)?;
    let raw: Vec<ContainerRaw> =
        serde_json::from_value(json).context("parse storage container list response")?;
    Ok(raw
        .into_iter()
        .map(|c| {
            let p = c.properties.unwrap_or_default();
            Entry::Container(ContainerEntry {
                name: c.name,
                last_modified: p.last_modified.unwrap_or_default(),
                public_access: p.public_access,
                lease_status: p.lease_status,
            })
        })
        .collect())
}

/// List blobs (and pseudo-folders via `--delimiter /`) in a container.
/// `az storage blob list --account-name <a> --container-name <c>
///  --prefix <p> --delimiter / --output json`.
pub fn list_blobs(account: &str, container: &str, prefix: &str) -> Result<Vec<Entry>> {
    let mut args: Vec<String> = vec![
        "storage".into(),
        "blob".into(),
        "list".into(),
        "--account-name".into(),
        account.into(),
        "--container-name".into(),
        container.into(),
        "--auth-mode".into(),
        "login".into(),
        "--delimiter".into(),
        "/".into(),
        "--num-results".into(),
        "1000".into(),
    ];
    if !prefix.is_empty() {
        args.push("--prefix".into());
        args.push(prefix.into());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let json = run_az(&arg_refs)?;

    let raw: Vec<BlobRaw> = serde_json::from_value(json).context("parse blob list response")?;
    let mut out: Vec<Entry> = Vec::new();
    for b in raw {
        // When `--delimiter /` is set, "virtual directories" appear
        // as entries with no properties.contentLength (the `name`
        // ends with `/`). Concrete blobs have a properties block.
        if b.name == prefix {
            continue;
        }
        let short = b.name.strip_prefix(prefix).unwrap_or(&b.name).to_string();
        match b.properties {
            Some(p) => {
                let content_settings = p.content_settings.unwrap_or_default();
                out.push(Entry::Blob(BlobEntry {
                    name: short,
                    full_name: b.name,
                    size: p.content_length.unwrap_or(0),
                    last_modified: p.last_modified.unwrap_or_default(),
                    content_type: content_settings.content_type,
                    blob_tier: p.blob_tier,
                }));
            }
            None => {
                // Pseudo-folder — name ends with `/`.
                out.push(Entry::Prefix(PrefixEntry { name: short }));
            }
        }
    }
    Ok(out)
}

/// Download a blob to a local path.
/// `az storage blob download --account-name <a> --container-name <c>
///  --name <blob> --file <out>`.
pub fn download(account: &str, container: &str, blob: &str, dest: &Path) -> Result<PathBuf> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    let dest_s = dest.to_string_lossy().to_string();
    let args = [
        "storage",
        "blob",
        "download",
        "--account-name",
        account,
        "--container-name",
        container,
        "--name",
        blob,
        "--auth-mode",
        "login",
        "--file",
        &dest_s,
    ];
    run_az_void(&args)?;
    Ok(dest.to_path_buf())
}

/// Upload a local file to a blob path.
/// `az storage blob upload --account-name <a> --container-name <c>
///  --name <blob> --file <local>`.
#[allow(dead_code)]
pub fn upload(local: &Path, account: &str, container: &str, blob: &str) -> Result<()> {
    let local_s = local.to_string_lossy().to_string();
    let args = [
        "storage",
        "blob",
        "upload",
        "--account-name",
        account,
        "--container-name",
        container,
        "--name",
        blob,
        "--auth-mode",
        "login",
        "--file",
        &local_s,
    ];
    run_az_void(&args)?;
    Ok(())
}

/// Delete one blob.
/// `az storage blob delete --account-name <a> --container-name <c>
///  --name <blob>`. Caller gates this behind a confirmation prompt.
pub fn delete(account: &str, container: &str, blob: &str) -> Result<()> {
    let args = [
        "storage",
        "blob",
        "delete",
        "--account-name",
        account,
        "--container-name",
        container,
        "--name",
        blob,
        "--auth-mode",
        "login",
    ];
    run_az_void(&args)?;
    Ok(())
}

/// Generate a short-lived blob SAS URL (read-only, 5 min).
/// Two-step under the hood:
///   1. `az storage blob generate-sas --output tsv` → SAS query string
///   2. compose `https://<account>.blob.core.windows.net/<container>/<blob>?<sas>`
pub fn presign(account: &str, container: &str, blob: &str) -> Result<String> {
    // Use UTC now + 5 minutes as the expiry. `az` accepts the
    // `YYYY-MM-DDTHH:MMZ` format.
    let now = chrono::Utc::now() + chrono::Duration::minutes(5);
    let expiry = now.format("%Y-%m-%dT%H:%MZ").to_string();
    let args = [
        "storage",
        "blob",
        "generate-sas",
        "--account-name",
        account,
        "--container-name",
        container,
        "--name",
        blob,
        "--permissions",
        "r",
        "--expiry",
        &expiry,
        "--https-only",
        "--auth-mode",
        "login",
        "--as-user",
    ];
    let sas = run_az_text(&args)?;
    // `az` returns the SAS as a quoted JSON string when --output is
    // json; we use tsv-equivalent via the default which prints raw.
    let sas = sas.trim().trim_matches('"').to_string();
    Ok(format!(
        "https://{account}.blob.core.windows.net/{container}/{blob}?{sas}"
    ))
}

/// Returns the Azure Portal URL for an account / container / blob.
/// Used by the `o` keybinding (open-in-browser).
///
/// Portal links use the storage account's full resource ID, which
/// includes the subscription. v0.1 just opens the storage-accounts
/// landing page filtered to the account name — good enough for an
/// "open in portal" jumping-off point.
pub fn portal_url(account: Option<&str>, container: Option<&str>, blob: Option<&str>) -> String {
    match (account, container, blob) {
        (Some(a), Some(c), Some(b)) => {
            // Direct container blade with the blob name in the query.
            format!(
                "https://portal.azure.com/#blade/Microsoft_Azure_Storage/ContainerMenuBlade/overview/storageAccountId//path/{c}/etag//?blob={}",
                urlencode(b)
            )
            .replace("//storageAccountId//", &format!("//storageAccountId/{a}/"))
        }
        (Some(a), Some(c), None) => format!(
            "https://portal.azure.com/#blade/Microsoft_Azure_Storage/ContainerMenuBlade/overview/storageAccountId/{a}/path/{c}"
        ),
        (Some(a), None, None) => format!(
            "https://portal.azure.com/#@/resource/subscriptions//resourceGroups//providers/Microsoft.Storage/storageAccounts/{a}/overview"
        ),
        _ => "https://portal.azure.com/#blade/HubsExtension/BrowseResource/resourceType/Microsoft.Storage%2FstorageAccounts".to_string(),
    }
}

/// Returns the `https://<account>.blob.core.windows.net/...` URL
/// for a blob (or container or account). Used for `Y` yank.
pub fn https_url(account: &str, container: Option<&str>, blob: Option<&str>) -> String {
    match (container, blob) {
        (Some(c), Some(b)) => format!("https://{account}.blob.core.windows.net/{c}/{b}"),
        (Some(c), None) => format!("https://{account}.blob.core.windows.net/{c}"),
        (None, _) => format!("https://{account}.blob.core.windows.net/"),
    }
}

// ── raw JSON shapes from `az storage ...` ──────────────────────

#[derive(Debug, Deserialize)]
struct AccountRaw {
    name: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(rename = "resourceGroup", default)]
    resource_group: Option<String>,
    #[serde(rename = "primaryEndpoints", default)]
    primary_endpoints: Option<EndpointsRaw>,
    #[serde(default)]
    sku: Option<SkuRaw>,
}

#[derive(Debug, Deserialize)]
struct EndpointsRaw {
    #[serde(default)]
    blob: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkuRaw {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ContainerRaw {
    name: String,
    #[serde(default)]
    properties: Option<ContainerPropsRaw>,
}

#[derive(Debug, Default, Deserialize)]
struct ContainerPropsRaw {
    #[serde(rename = "lastModified", default)]
    last_modified: Option<String>,
    #[serde(rename = "publicAccess", default)]
    public_access: Option<String>,
    #[serde(rename = "leaseStatus", default)]
    lease_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlobRaw {
    name: String,
    #[serde(default)]
    properties: Option<BlobPropsRaw>,
}

#[derive(Debug, Deserialize)]
struct BlobPropsRaw {
    #[serde(rename = "contentLength", default)]
    content_length: Option<u64>,
    #[serde(rename = "lastModified", default)]
    last_modified: Option<String>,
    #[serde(rename = "contentSettings", default)]
    content_settings: Option<ContentSettingsRaw>,
    #[serde(rename = "blobTier", default)]
    blob_tier: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ContentSettingsRaw {
    #[serde(rename = "contentType", default)]
    content_type: Option<String>,
}

// ── `az` subprocess helpers ─────────────────────────────────────

fn run_az(args: &[&str]) -> Result<serde_json::Value> {
    let mut cmd = Command::new("az");
    cmd.args(args).arg("--output").arg("json");
    let out = cmd
        .output()
        .map_err(|e| anyhow!("spawn az: {e} — is the Azure CLI on PATH?"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "az {} → {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if out.stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("parse json: {e}"))
}

fn run_az_void(args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("az");
    cmd.args(args);
    let out = cmd
        .output()
        .map_err(|e| anyhow!("spawn az: {e} — is the Azure CLI on PATH?"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "az {} → {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn run_az_text(args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("az");
    cmd.args(args);
    let out = cmd
        .output()
        .map_err(|e| anyhow!("spawn az: {e} — is the Azure CLI on PATH?"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "az {} → {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn urlencode(s: &str) -> String {
    // Minimal URL encoding — same scheme as fs-s3 (slash + safe
    // alphas pass through; spaces / non-ASCII get percent-encoded).
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
    fn https_url_for_blob() {
        let u = https_url("myacct", Some("logs"), Some("2026/06/build-log.txt"));
        assert_eq!(
            u,
            "https://myacct.blob.core.windows.net/logs/2026/06/build-log.txt"
        );
    }

    #[test]
    fn https_url_for_container() {
        let u = https_url("myacct", Some("logs"), None);
        assert_eq!(u, "https://myacct.blob.core.windows.net/logs");
    }

    #[test]
    fn portal_url_for_account_only() {
        let u = portal_url(Some("myacct"), None, None);
        assert!(u.contains("storageAccounts/myacct"));
    }
}
