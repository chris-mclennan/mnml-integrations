//! Config file at `~/.config/mnml-virt-docker/config.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// Tab kind:
    /// - `containers` — every container in the local engine
    /// - `images`     — every image
    /// - `volumes`    — every volume
    /// - `networks`   — every network
    /// - `compose`    — services in a single compose project (requires `project_path`)
    pub kind: String,
    /// Path to the directory containing the compose project's
    /// `docker-compose.yml` — required when `kind = "compose"`.
    #[serde(default)]
    pub project_path: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-virt-docker config. Edit and re-run.
#
# Auto-refresh interval for the active tab. Set to 0 to disable.
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "containers" — every container in the local engine
#   "images"     — every image
#   "volumes"    — every volume
#   "networks"   — every network
#   "compose"    — services in a single compose project (requires `project_path`)

[[tabs]]
name = "containers"
kind = "containers"

[[tabs]]
name = "images"
kind = "images"

[[tabs]]
name = "volumes"
kind = "volumes"

[[tabs]]
name = "networks"
kind = "networks"

# Example compose-project tab — uncomment + point at the project dir
# containing the docker-compose.yml:
# [[tabs]]
# name = "myapp"
# kind = "compose"
# project_path = "/Users/me/Projects/myapp"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "containers" | "images" | "volumes" | "networks" => {}
                "compose" => {
                    if t.project_path.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): kind=\"compose\" requires `project_path`",
                            t.name
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected containers/images/volumes/networks/compose)",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-virt-docker")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
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
        assert_eq!(cfg.tabs.len(), 4);
        // First-line defaults match the spec.
        assert_eq!(cfg.tabs[0].kind, "containers");
        assert_eq!(cfg.tabs[1].kind, "images");
        assert_eq!(cfg.tabs[2].kind, "volumes");
        assert_eq!(cfg.tabs[3].kind, "networks");
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
                project_path: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_compose_without_project_path() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "myapp".into(),
                kind: "compose".into(),
                project_path: None,
            }],
        };
        assert!(cfg.validate().is_err());

        let cfg_ok = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "myapp".into(),
                kind: "compose".into(),
                project_path: Some("/tmp/myapp".into()),
            }],
        };
        assert!(cfg_ok.validate().is_ok());
    }
}
