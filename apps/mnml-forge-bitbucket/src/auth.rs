//! Bitbucket Cloud token loader. Two auth flavors share the same
//! Basic-Auth wire format (`Basic base64(email:token)`) — the
//! difference lives on Atlassian's side, in which rate-limit bucket
//! the request draws from:
//!
//! - **Scoped API token** (Atlassian id.atlassian.com > API tokens).
//!   Preferred. Fresh bucket, plus you can revoke per-integration
//!   without invalidating the rest of your automation. Set
//!   `BITBUCKET_API_TOKEN`.
//! - **App password** (Bitbucket Cloud > App passwords). Legacy
//!   surface still supported, but shares its rate-limit bucket with
//!   every other tool that authenticates the same way. Set
//!   `BITBUCKET_APP_PASSWORD`.
//!
//! Resolution order (highest precedence first):
//!
//!   1. `BITBUCKET_API_TOKEN` — Atlassian scoped api_token.
//!   2. `BITBUCKET_APP_PASSWORD` — Bitbucket app password.
//!   3. `BITBUCKET_PERSONAL_TOKEN` — legacy combined `email:token`
//!      shape; strips the `email:` prefix. Accepts either kind of
//!      token.
//!   4. `~/.config/mnml-forge-bitbucket/token` — one-line, `chmod 600`.
//!
//! Create tokens at:
//!   - api_token:    <https://id.atlassian.com/manage-profile/security/api-tokens>
//!   - app password: <https://bitbucket.org/account/settings/app-passwords/>
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

/// Extract just the token portion from an `email:token` combined
/// string. Returns the input unchanged when no `:` is present
/// (assumes the caller already passed a bare token).
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
    // 1. `BITBUCKET_API_TOKEN` — Atlassian scoped api_token.
    //    Preferred: fresh rate-limit bucket independent of app
    //    passwords, per-integration revoke without invalidating
    //    other tooling.
    if let Ok(t) = std::env::var("BITBUCKET_API_TOKEN") {
        let t = strip_email_prefix(t.trim()).to_string();
        if !t.is_empty() {
            return Ok((t, TokenSource::Env("BITBUCKET_API_TOKEN")));
        }
    }
    // 2. `BITBUCKET_APP_PASSWORD` — legacy app password bucket.
    //    Bare token expected; strip prefix defensively in case a
    //    user exports `email:pw` here too.
    if let Ok(t) = std::env::var("BITBUCKET_APP_PASSWORD") {
        let t = strip_email_prefix(t.trim()).to_string();
        if !t.is_empty() {
            return Ok((t, TokenSource::Env("BITBUCKET_APP_PASSWORD")));
        }
    }
    // 3. `BITBUCKET_PERSONAL_TOKEN` — accept either bare or
    //    `email:token`. Matches the shape user shells often
    //    export for Bitbucket Cloud tooling. Works for either
    //    api_token or app_password since the wire format is
    //    identical.
    if let Ok(t) = std::env::var("BITBUCKET_PERSONAL_TOKEN") {
        let t = strip_email_prefix(t.trim()).to_string();
        if !t.is_empty() {
            return Ok((t, TokenSource::Env("BITBUCKET_PERSONAL_TOKEN")));
        }
    }
    // 4. Fallback: the on-disk token file.
    let path = token_path();
    let s = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {} (also tried env: BITBUCKET_API_TOKEN, BITBUCKET_APP_PASSWORD, BITBUCKET_PERSONAL_TOKEN)",
            path.display()
        )
    })?;
    let token = s.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!(
            "{} is empty — paste your Bitbucket api_token or app password, or set BITBUCKET_API_TOKEN / BITBUCKET_APP_PASSWORD",
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
        assert_eq!(strip_email_prefix("me@example.com:ATATT-abc"), "ATATT-abc");
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
