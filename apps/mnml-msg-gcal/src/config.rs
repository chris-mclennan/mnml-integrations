//! Config — `~/.config/mnml-msg-gcal.toml`. First-run scaffolds a
//! commented template; subsequent runs merge user values.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Which Google Calendar to browse. `"primary"` for the
    /// user's primary calendar; otherwise the calendar id (an
    /// email-shaped string from the Calendar list).
    pub calendar_id: String,
    /// Timezone for rendering event start/end times. Defaults to
    /// `$TZ` when set, otherwise `"UTC"`.
    pub timezone: String,
    /// Auto-refresh cadence in seconds. `0` disables auto-refresh
    /// (manual `r` only).
    pub refresh_secs: u64,
    /// Days ahead of `today` the "upcoming" pane surfaces. Common
    /// values: 7 (a week), 14, 30.
    pub upcoming_days: u32,
}

impl Default for Config {
    fn default() -> Self {
        let timezone = std::env::var("TZ").unwrap_or_else(|_| "UTC".to_string());
        Self {
            calendar_id: "primary".into(),
            timezone,
            refresh_secs: 60,
            upcoming_days: 14,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
        .join("mnml-msg-gcal.toml")
}

/// Load the config. First-run creates a scaffold + returns it.
pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        write_scaffold(&path)?;
        return Ok(Config::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg)
}

fn write_scaffold(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let scaffold = r#"# mnml-msg-gcal — Google Calendar client
#
# Which calendar to browse. Use "primary" for your default
# calendar, or paste a calendar id (email-shaped, from the
# Calendar list). Common alt values: your work email, a shared
# team calendar's id, etc.
calendar_id = "primary"

# Timezone for rendering times. Defaults to $TZ when set,
# otherwise "UTC".
timezone = "UTC"

# Auto-refresh cadence in seconds. 0 = manual only (press r).
refresh_secs = 60

# Days ahead surfaced in the "upcoming" pane.
upcoming_days = 14
"#;
    std::fs::write(path, scaffold)
        .with_context(|| format!("write scaffold to {}", path.display()))?;
    Ok(())
}
