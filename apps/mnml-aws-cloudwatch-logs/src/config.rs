//! Config file at `~/.config/mnml-aws-cloudwatch-logs.toml`. First
//! run writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Optional default region — overridden per-tab via `region`.
    /// Defers to the AWS CLI's resolution chain when unset.
    #[serde(default)]
    pub region: Option<String>,
    /// Tabs auto-refresh is N/A here — log tail is already live.
    /// Field kept for forward-compat with the family idiom.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// CloudWatch log group name (e.g. `/aws/lambda/my-func`).
    pub log_group: String,
    /// Optional log stream filter — narrows the tail to one stream
    /// instead of all streams in the group.
    #[serde(default)]
    pub log_stream: Option<String>,
    /// Optional region override for this tab.
    #[serde(default)]
    pub region: Option<String>,
    /// Optional CloudWatch Logs filter pattern. Passed to `aws logs
    /// tail --filter-pattern` directly. Use cases:
    ///   - `"ERROR"` — substring match
    ///   - `'{ $.level = "error" }'` — JSON field match
    /// See https://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/FilterAndPatternSyntax.html
    #[serde(default)]
    pub filter: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-cloudwatch-logs config. Edit and re-run.
#
# Optional top-level region (defers to AWS CLI when unset):
# region = "us-east-1"

refresh_interval_secs = 0

# ── Tabs ─────────────────────────────────────────────────────────
# Each [[tabs]] entry is one log group + optional stream filter.
# Switch with 1-9 in the TUI; each tab spawns `aws logs tail --follow`
# on first activation and keeps the child running until close.

[[tabs]]
name = "lambda errors"
log_group = "/aws/lambda/my-function"
# Optional: narrow to one stream
# log_stream = "2026/06/06/[$LATEST]abc123"
# Optional: filter pattern (substring or CloudWatch Logs syntax)
filter = "ERROR"

[[tabs]]
name = "api gateway"
log_group = "/aws/apigateway/my-api"

[[tabs]]
name = "ecs service"
log_group = "/ecs/my-service"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            if t.log_group.trim().is_empty() {
                return Err(anyhow!("tab #{i} ({}): `log_group` is required", t.name));
            }
        }
        Ok(())
    }

    /// Build a single-tab Config from a CLI `--log-group` invocation —
    /// the cross-sibling handoff path. Bypasses the on-disk config
    /// entirely (the user's regular tabs aren't touched).
    ///
    /// `name` defaults to the log group's last path segment so a
    /// Lambda function's `/aws/lambda/api-handler` group renders as
    /// `api-handler` in the tab strip.
    pub fn one_off_tab(
        log_group: String,
        name: Option<String>,
        filter: Option<String>,
        region: Option<String>,
    ) -> Self {
        let label = name.unwrap_or_else(|| {
            log_group
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(&log_group)
                .to_string()
        });
        Config {
            region: region.clone(),
            refresh_interval_secs: 0,
            tabs: vec![Tab {
                name: label,
                log_group,
                log_stream: None,
                region,
                filter,
            }],
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-aws-cloudwatch-logs.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        return Err(anyhow!(
            "wrote config template to {} — edit it then re-run",
            path.display()
        ));
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
        assert!(cfg.tabs.len() >= 3);
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 0,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn one_off_tab_derives_label_from_last_path_segment() {
        let cfg = Config::one_off_tab("/aws/lambda/api-handler".to_string(), None, None, None);
        assert_eq!(cfg.tabs.len(), 1);
        assert_eq!(cfg.tabs[0].name, "api-handler");
        assert_eq!(cfg.tabs[0].log_group, "/aws/lambda/api-handler");
    }

    #[test]
    fn one_off_tab_uses_explicit_name_when_given() {
        let cfg = Config::one_off_tab(
            "/aws/lambda/x".to_string(),
            Some("My Lambda".to_string()),
            None,
            None,
        );
        assert_eq!(cfg.tabs[0].name, "My Lambda");
    }

    #[test]
    fn one_off_tab_threads_filter_and_region() {
        let cfg = Config::one_off_tab(
            "/g".to_string(),
            None,
            Some("ERROR".to_string()),
            Some("us-west-2".to_string()),
        );
        assert_eq!(cfg.region.as_deref(), Some("us-west-2"));
        assert_eq!(cfg.tabs[0].filter.as_deref(), Some("ERROR"));
        assert_eq!(cfg.tabs[0].region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn one_off_tab_validates_clean() {
        let cfg = Config::one_off_tab("/x".to_string(), None, None, None);
        cfg.validate().expect("one-off tab must validate");
    }

    #[test]
    fn rejects_empty_log_group() {
        let cfg = Config {
            region: None,
            refresh_interval_secs: 0,
            tabs: vec![Tab {
                name: "bad".into(),
                log_group: "".into(),
                log_stream: None,
                region: None,
                filter: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
