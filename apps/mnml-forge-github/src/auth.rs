//! GitHub personal access token loader. Reads
//! `~/.config/mnml-forge-github/token` (one line, `chmod 600`).
//!
//! Generate at: https://github.com/settings/tokens (classic, `repo`
//! scope for private; `public_repo` is enough for public-only).
//! Fine-grained PATs also work — give Issues + Pull requests read
//! access on the repos you want to query.
//!
//! Falls back to the `GITHUB_TOKEN_WORK` / `GITHUB_TOKEN` env vars
//! when the token file doesn't exist, for users who keep a
//! well-known token env var in their shell (e.g. `~/.zshenv`)
//! instead of a per-machine config file.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

pub fn token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-forge-github")
        .join("token")
}

pub fn load_token() -> Result<String> {
    let path = token_path();
    if path.exists() {
        let s = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let token = s.trim().to_string();
        if token.is_empty() {
            return Err(anyhow!(
                "{} is empty — paste your GitHub PAT",
                path.display()
            ));
        }
        return Ok(token);
    }

    if let Ok(token) = std::env::var("GITHUB_TOKEN_WORK") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    Err(anyhow!(
        "no GitHub token found — save one to {} (chmod 600), or set \
         GITHUB_TOKEN_WORK or GITHUB_TOKEN in your shell env as an alternative",
        path.display()
    ))
}
