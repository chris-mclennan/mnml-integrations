//! Config file at `~/.config/mnml-msg-slack/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`SLACK_USER_TOKEN`, optional
//! `SLACK_BOT_TOKEN`) — never in the TOML.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default)]
    pub post_multiline: bool,
    #[serde(default)]
    pub tabs: Vec<Tab>,
    /// Channel-visibility filter for the `channels` tab.
    /// Slack orgs commonly have 50–200 channels; without a
    /// filter the tab is a wall of noise. See `ChannelFilter`.
    #[serde(default)]
    pub channels: ChannelFilter,
}

/// Two-sided filter on channel names. `show` (when non-empty)
/// restricts the visible list to entries in the array; `hide`
/// always excludes and wins over `show`. Names may include or
/// omit the leading `#`; matching is case-insensitive on the
/// bare name.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelFilter {
    #[serde(default)]
    pub show: Vec<String>,
    #[serde(default)]
    pub hide: Vec<String>,
    /// Pinned channels — always sorted to the TOP of the list in
    /// the order given. Independent of `show`/`hide`: pinned rows
    /// are shown unconditionally, and are dropped from `pin`
    /// (not `hide`) when the user un-pins.
    #[serde(default)]
    pub pin: Vec<String>,
}

impl ChannelFilter {
    fn norm(s: &str) -> String {
        s.trim().trim_start_matches('#').to_lowercase()
    }

    /// True when the channel with `bare_name` (no leading `#`)
    /// should be included in the visible list. Pinned channels
    /// always pass — they take precedence over `hide`.
    pub fn allows(&self, bare_name: &str) -> bool {
        let n = Self::norm(bare_name);
        if self.pin.iter().any(|p| Self::norm(p) == n) {
            return true;
        }
        if self.hide.iter().any(|h| Self::norm(h) == n) {
            return false;
        }
        if self.show.is_empty() {
            return true;
        }
        self.show.iter().any(|s| Self::norm(s) == n)
    }

    /// Sort-key position (0..pin.len()) if pinned, else None.
    /// Callers use this to move pinned rows to the top of the
    /// list in `pin` order.
    pub fn pin_position(&self, bare_name: &str) -> Option<usize> {
        let n = Self::norm(bare_name);
        self.pin.iter().position(|p| Self::norm(p) == n)
    }

    /// 2026-08-01 — position in `show` (0..show.len()) so
    /// sort_channels can respect user-declared order for
    /// non-pinned rows. None when `show` is empty or the
    /// channel isn't in it. Case-insensitive on the bare name.
    pub fn show_position(&self, bare_name: &str) -> Option<usize> {
        if self.show.is_empty() {
            return None;
        }
        let n = Self::norm(bare_name);
        self.show.iter().position(|s| Self::norm(s) == n)
    }

    /// Case-insensitive `bare` match against the pin list.
    pub fn is_pinned(&self, bare_name: &str) -> bool {
        self.pin_position(bare_name).is_some()
    }
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// Tab kind:
    ///   - `channels` — public + private channels, sorted by membership / unread
    ///   - `dms` — direct messages + multi-person DMs
    ///   - `search` — interactive query input (search.messages)
    ///   - `threads` — v0.1 stub
    pub kind: String,
    /// Reserved for v0.2 (per-tab filters / query presets).
    #[serde(default)]
    pub query: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-msg-slack config. Edit and re-run.
#
# Auth lives in env vars (NOT here):
#   export SLACK_USER_TOKEN=xoxp-...   (required — user token)
#   export SLACK_BOT_TOKEN=xoxb-...    (optional — falls back to user)
#
# Create a Slack app at https://api.slack.com/apps, install it to
# your workspace, request the User-token scopes listed in the
# README, then copy the User OAuth Token.

refresh_interval_secs = 60
post_multiline = false

# ── Channel filter ──────────────────────────────────────────────
# Slack orgs commonly have 50-200 channels. The `show` list is
# a whitelist — when non-empty, ONLY those channels appear in
# the channels tab, in the order you list them here (your order
# wins; alphabetical is only the fallback for un-listed rows).
# Leave `show = []` to see every channel your token has access
# to.
#
# `hide` always excludes and wins over `show`. `pin` sorts a
# channel to the very top of the list in the order you give.
#
# Names may include or omit the leading `#`. Matching is
# case-insensitive.
#
# Example — trim a noisy org down to just the ones you care about:
#   [channels]
#   show = [
#     "eng-general",
#     "team-tattle",
#     "deploys",
#   ]
#
# Discover names with `mnml-msg-slack --list-channels`. Press
# `x` on a channel to toast a "hidden — add to [channels].hide"
# hint.
#
[channels]
show = []
hide = []

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "channels" — public + private channels
#   "dms"      — direct messages + group DMs
#   "search"   — interactive search.messages query
#   "threads"  — v0.1 stub

[[tabs]]
name = "channels"
kind = "channels"

[[tabs]]
name = "dms"
kind = "dms"

[[tabs]]
name = "search"
kind = "search"

[[tabs]]
name = "threads"
kind = "threads"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "channels" | "dms" | "search" | "threads" | "canvases" => {}
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"channels\", \"dms\", \"search\", or \"threads\")",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-msg-slack")
        .join("config.toml")
}

/// Append a channel name to `[channels].hide` in the on-disk
/// config, preserving all comments + user formatting. Used by the
/// `x` hide-channel hotkey.
///
/// Uses `toml_edit` so a single one-key gesture doesn't nuke the
/// heavily-commented shipped template (was previously a full
/// re-serialize via `toml::to_string_pretty` — 2026-07-22
/// HIGH-severity tester finding). Atomic write via a `.tmp`
/// sibling + rename so a crash mid-write can't leave a corrupt
/// config.
pub fn append_hide_channel(name: &str) -> Result<()> {
    append_channel_at(name, "hide", &config_path())
}

#[cfg(test)]
pub fn append_hide_channel_at(name: &str, path: &std::path::Path) -> Result<()> {
    append_channel_at(name, "hide", path)
}

/// Append `#name` to `[channels].pin` — same idempotent + comment-
/// preserving pattern as the hide variant.
pub fn append_pin_channel(name: &str) -> Result<()> {
    append_channel_at(name, "pin", &config_path())
}

/// Remove `name` (case-insensitive, `#` optional) from `[channels].pin`.
pub fn remove_pin_channel(name: &str) -> Result<()> {
    remove_channel_at(name, "pin", &config_path())
}

fn remove_channel_at(name: &str, list_key: &str, path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let bare = name.trim_start_matches('#').to_lowercase();
    if let Some(channels) = doc.get_mut("channels").and_then(|c| c.as_table_mut())
        && let Some(list) = channels
            .get_mut(list_key)
            .and_then(|v| v.as_value_mut())
            .and_then(|v| v.as_array_mut())
    {
        list.retain(|v| {
            v.as_str()
                .map(|s| s.trim_start_matches('#').to_lowercase() != bare)
                .unwrap_or(true)
        });
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn append_channel_at(name: &str, list_key: &str, path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Load existing text (or the shipped template on first run).
    let text = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        Config::EXAMPLE.to_string()
    };
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    // Ensure `[channels]` table exists, then push onto the target list.
    let channels = doc
        .entry("channels")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`channels` is not a table"))?;
    let list = channels
        .entry(list_key)
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_value_mut()
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("`channels.{list_key}` is not an array"))?;
    let bare = name.trim_start_matches('#');
    let with_hash = format!("#{bare}");
    // Idempotent: skip if already present (case-insensitive on
    // bare name, matching `ChannelFilter::allows`).
    let already = list.iter().any(|v| {
        v.as_str()
            .map(|s| s.trim_start_matches('#').eq_ignore_ascii_case(bare))
            .unwrap_or(false)
    });
    if !already {
        list.push(with_hash);
    }
    // Atomic write.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load() -> Result<Config> {
    let path = config_path();
    let first_run = !path.exists();
    if first_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        println!(
            "first run: wrote config template to {} — edit it to customize",
            path.display(),
        );
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert!(!cfg.tabs.is_empty());
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            refresh_interval_secs: 60,
            post_multiline: false,
            tabs: vec![],
            channels: ChannelFilter::default(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 60,
            post_multiline: false,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
                query: None,
            }],
            channels: ChannelFilter::default(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_all_known_kinds() {
        for kind in &["channels", "dms", "search", "threads"] {
            let cfg = Config {
                refresh_interval_secs: 60,
                post_multiline: false,
                tabs: vec![Tab {
                    name: "x".into(),
                    kind: kind.to_string(),
                    query: None,
                }],
                channels: ChannelFilter::default(),
            };
            assert!(cfg.validate().is_ok(), "expected `{kind}` to validate");
        }
    }

    #[test]
    fn append_hide_channel_preserves_comments_and_dedupes() {
        // Start with a heavily-commented user file — same shape as
        // the shipped template. `append_hide_channel_at` must keep
        // every comment intact and append idempotently.
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# Header comment we care about\nrefresh_interval_secs = 60\n\n# ── Channels ──\n[channels]\n# noisy channels go here\nhide = [\"#politics\"]\n",
        )
        .unwrap();
        // First append — adds #random.
        append_hide_channel_at("random", &path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# Header comment we care about"), "header comment survives:\n{after}");
        assert!(after.contains("# noisy channels go here"), "section comment survives:\n{after}");
        assert!(after.contains("#politics"), "existing hide entry preserved:\n{after}");
        assert!(after.contains("#random"), "new hide appended:\n{after}");

        // Second append with the same name — must be idempotent.
        append_hide_channel_at("#random", &path).unwrap();
        let after2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after2.matches("#random").count(),
            1,
            "second append should NOT duplicate:\n{after2}"
        );
    }
}
