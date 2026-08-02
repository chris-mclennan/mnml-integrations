//! Azure DevOps PAT loader. Reads
//! `~/.config/mnml-forge-azdevops/token` (one line, `chmod 600`).
//!
//! Create a PAT at:
//!   `https://dev.azure.com/<org>/_usersSettings/tokens`
//!
//! Minimum scopes: **Code (Read)** for PR tabs and **Build (Read)**
//! for build tabs. Add **User Profile (Read)** for `mode = "mine"` /
//! `mode = "reviewing"` (those resolve the current user's `id` via
//! `/_apis/connectionData` at startup).

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

pub fn token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-forge-azdevops")
        .join("token")
}

pub fn load_token() -> Result<String> {
    let path = token_path();
    let s =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let token = s.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!(
            "{} is empty — paste your Azure DevOps PAT",
            path.display()
        ));
    }
    Ok(token)
}
