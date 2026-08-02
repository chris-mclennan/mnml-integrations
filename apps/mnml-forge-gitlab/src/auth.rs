//! GitLab Personal Access Token loader. Reads
//! `~/.config/mnml-forge-gitlab/token` (one line, `chmod 600`).
//!
//! Create a PAT at:
//!   `https://gitlab.com/-/user_settings/personal_access_tokens`
//!   (or `https://<self-hosted-gitlab>/-/user_settings/personal_access_tokens`)
//!
//! Minimum scopes: **read_api** is sufficient for everything the
//! viewer does (list MRs, list pipelines, resolve the current user).
//! `api` works too but is broader than needed.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

pub fn token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-forge-gitlab")
        .join("token")
}

pub fn load_token() -> Result<String> {
    let path = token_path();
    let s =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let token = s.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!(
            "{} is empty — paste your GitLab personal access token",
            path.display()
        ));
    }
    Ok(token)
}
