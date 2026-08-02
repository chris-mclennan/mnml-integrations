//! Auth — the Jira API token lives in
//! `~/.config/mnml-tracker-jira/token`. Generated at:
//!   https://id.atlassian.com/manage-profile/security/api-tokens
//!
//! The HTTP layer uses HTTP Basic auth with `email:token`.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

/// Location of the API token file.
pub fn token_path() -> PathBuf {
    // Use `~/.config/` everywhere — see comment in `config.rs`.
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-tracker-jira")
        .join("token")
}

/// Read the API token. Errors include a clear hint to create one.
pub fn load_token() -> Result<String> {
    let p = token_path();
    if !p.exists() {
        return Err(anyhow!(
            "missing Jira API token.\n\
             Generate one at https://id.atlassian.com/manage-profile/security/api-tokens\n\
             and save it (chmod 600) to:\n\
               {}",
            p.display(),
        ));
    }
    let raw = std::fs::read_to_string(&p)
        .with_context(|| format!("reading token from {}", p.display()))?;
    // 2026-07-25 — strip surrounding quotes on top of whitespace.
    // A user reported all searches returning 0 issues; the file
    // had `"<token>"` (from pasting Atlassian's copy-with-quotes
    // UI), which auth'd as a corrupted token that quietly
    // returned zero-scope results instead of 401'ing.
    let token = raw
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'')
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(anyhow!("token file {} is empty", p.display()));
    }
    Ok(token)
}
