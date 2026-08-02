//! Bitbucket Cloud app-password loader. Resolves the app password
//! from (in order):
//!
//!   1. `BITBUCKET_APP_PASSWORD` env var — the app password on its own.
//!   2. `BITBUCKET_PERSONAL_TOKEN` env var — either the app password on
//!      its own, or the combined `email:app_password` form (in which
//!      case only the suffix after `:` is used). This matches the
//!      shape most Atlassian-CLI tooling exports.
//!   3. `~/.config/mnml-forge-bitbucket/token` — one line, `chmod 600`.
//!
//! Create an app password at:
//!   <https://bitbucket.org/account/settings/app-passwords/>
//!
//! Minimum scopes: **Pull requests: Read**. Add **Account: Read** if
//! you want `mode = "mine"` / `mode = "reviewing"` tabs (those need
//! `/2.0/user` to resolve your account_id).

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

pub fn token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-forge-bitbucket")
        .join("token")
}

/// Extract just the app-password portion from an
/// `email:app_password` combined string. Returns the input
/// unchanged when no `:` is present (assumes the caller already
/// passed a bare token).
fn strip_email_prefix(s: &str) -> &str {
    match s.split_once(':') {
        Some((_email, pw)) => pw,
        None => s,
    }
}

/// Where a loaded token came from — surfaced in the `--check`
/// output so users can tell whether the file or an env var is
/// currently authoritative.
pub enum TokenSource {
    Env(&'static str),
    File(PathBuf),
}

impl std::fmt::Display for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenSource::Env(name) => write!(f, "env: {name}"),
            TokenSource::File(p) => write!(f, "{}", p.display()),
        }
    }
}

pub fn load_token() -> Result<(String, TokenSource)> {
    // 1. Explicit `BITBUCKET_APP_PASSWORD` env — the least ambiguous
    //    signal. Users who set this variable have already committed
    //    to the "bare app password" convention.
    if let Ok(t) = std::env::var("BITBUCKET_APP_PASSWORD") {
        let t = t.trim();
        if !t.is_empty() {
            return Ok((t.to_string(), TokenSource::Env("BITBUCKET_APP_PASSWORD")));
        }
    }
    // 2. `BITBUCKET_PERSONAL_TOKEN` — accept either bare or
    //    `email:app_password`. Matches the shape user shells often
    //    export for Bitbucket Cloud tooling.
    if let Ok(t) = std::env::var("BITBUCKET_PERSONAL_TOKEN") {
        let t = strip_email_prefix(t.trim()).to_string();
        if !t.is_empty() {
            return Ok((t, TokenSource::Env("BITBUCKET_PERSONAL_TOKEN")));
        }
    }
    // 3. Fallback: the on-disk token file.
    let path = token_path();
    let s = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {} (also tried env: BITBUCKET_APP_PASSWORD, BITBUCKET_PERSONAL_TOKEN)",
            path.display()
        )
    })?;
    let token = s.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!(
            "{} is empty — paste your Bitbucket app password, or set BITBUCKET_APP_PASSWORD",
            path.display()
        ));
    }
    Ok((token, TokenSource::File(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_email_prefix_splits_on_colon() {
        assert_eq!(
            strip_email_prefix("me@example.com:ATATT-abc"),
            "ATATT-abc"
        );
    }

    #[test]
    fn strip_email_prefix_passes_through_bare_token() {
        assert_eq!(strip_email_prefix("ATATT-abc"), "ATATT-abc");
    }

    #[test]
    fn strip_email_prefix_handles_empty_after_colon() {
        assert_eq!(strip_email_prefix("me@example.com:"), "");
    }
}
