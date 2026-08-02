//! App state — per-tab list of ECR items (repositories OR images) +
//! a selection cursor.

use crate::config::{Config, Tab};
use crate::ecr::{self, Item};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    pub repository: Option<String>,
    pub region: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match t.kind.as_str() {
            "repositories" => Ok(Self {
                kind: "repositories".into(),
                repository: None,
                region,
            }),
            "images" => {
                let repo = t.repository.clone().unwrap_or_default();
                if repo.trim().is_empty() {
                    anyhow::bail!("tab `{}`: kind=\"images\" requires `repository`", t.name);
                }
                Ok(Self {
                    kind: "images".into(),
                    repository: Some(repo),
                    region,
                })
            }
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

pub struct ItemsTab {
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
}

impl ItemsTab {
    fn empty() -> Self {
        ItemsTab {
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: ItemsTab,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t, cfg.region.as_deref())?;
            tabs.push(TabState {
                name: t.name.clone(),
                data: ItemsTab::empty(),
                spec,
            });
        }
        let mut app = App {
            cfg,
            tabs,
            active_tab: 0,
            status: String::new(),
        };
        app.refresh_active();
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            if self.tabs[idx].data.items.is_empty() && self.tabs[idx].data.last_error.is_none() {
                self.refresh_active();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.data.items.is_empty() {
            return;
        }
        let n = tab.data.items.len() as isize;
        let cur = tab.data.selected as isize;
        let next = (cur + delta).clamp(0, n - 1);
        tab.data.selected = next as usize;
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        self.status = format!("loading {name}…");
        self.tabs[idx].data.loading = true;

        let result: Result<Vec<Item>> = match spec.kind.as_str() {
            "repositories" => ecr::describe_repositories(spec.region.as_deref())
                .map(|rs| rs.into_iter().map(Item::Repository).collect()),
            "images" => {
                let repo = spec
                    .repository
                    .as_deref()
                    .expect("images tab requires repository (validated)");
                ecr::describe_images(repo, spec.region.as_deref())
                    .map(|is| is.into_iter().map(Item::Image).collect())
            }
            _ => unreachable!("validated in TabSpec::resolve"),
        };

        let t = &mut self.tabs[idx];
        t.data.loading = false;
        match result {
            Ok(items) => {
                let count = items.len();
                t.data.items = items;
                t.data.selected = t.data.selected.min(count.saturating_sub(1));
                t.data.last_loaded = Some(Instant::now());
                t.data.last_error = None;
                let kind_label = match spec.kind.as_str() {
                    "repositories" => "repositories",
                    "images" => "images",
                    _ => "items",
                };
                self.status = format!("{name}: {count} {kind_label}");
            }
            Err(e) => {
                t.data.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return false;
        }
        let idx = self.active_tab;
        let stale = match self.tabs[idx].data.last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].data.loading {
            self.refresh_active();
            true
        } else {
            false
        }
    }

    pub fn drain(&mut self) -> bool {
        false
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.data.items.get(t.data.selected)
    }

    pub fn open_console(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        let region = self.active().spec.region.as_deref().unwrap_or("us-east-1");
        let url = match item {
            Item::Repository(r) => format!(
                "https://{region}.console.aws.amazon.com/ecr/private-registry/repositories/private/{}/{}?region={region}",
                r.registry_id.as_deref().unwrap_or("registry"),
                r.name
            ),
            Item::Image(i) => format!(
                "https://{region}.console.aws.amazon.com/ecr/repositories/private/registry/{}/_/image?region={region}",
                i.repository_name
            ),
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    pub fn yank_arn(&mut self) {
        let Some(item) = self.focused_item() else {
            self.status = "no item under cursor".into();
            return;
        };
        // For repos: yank the ARN. For images: yank the qualified URI
        // (uri:tag or uri@digest) — what you'd `docker pull`.
        let payload = match item {
            Item::Repository(r) => r.arn.clone(),
            Item::Image(i) => image_pull_string(i),
        };
        if payload.is_empty() {
            self.status = "nothing to copy for this item".into();
            return;
        }
        match crate::clipboard::copy(&payload) {
            Ok(()) => {
                let label = match item {
                    Item::Repository(_) => "ARN",
                    Item::Image(_) => "image pull URI",
                };
                self.status = format!("copied {label} ({} chars)", payload.len());
            }
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }
}

/// Format an image as `<repository_uri>:<tag>` (preferring the first
/// tag) or `<repository_uri>@<digest>` for untagged images. The
/// repository URI isn't stored on `ImageDetail` directly, so we
/// reconstruct it from the digest's registry-host inference — but
/// for v0.1, just emit `<repo>:<tag>` or `<repo>@<digest>` since
/// the user's docker client already knows the registry.
fn image_pull_string(i: &crate::ecr::ImageDetail) -> String {
    let repo = &i.repository_name;
    if let Some(tag) = i.tags.first() {
        return format!("{repo}:{tag}");
    }
    if let Some(digest) = &i.digest {
        return format!("{repo}@{digest}");
    }
    repo.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;
    use crate::ecr::ImageDetail;

    #[test]
    fn tab_spec_resolve_uses_default_region() {
        let t = Tab {
            name: "x".into(),
            kind: "repositories".into(),
            repository: None,
            region: None,
        };
        let spec = TabSpec::resolve(&t, Some("us-west-2")).unwrap();
        assert_eq!(spec.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn tab_spec_rejects_images_without_repository() {
        let t = Tab {
            name: "bad".into(),
            kind: "images".into(),
            repository: None,
            region: None,
        };
        assert!(TabSpec::resolve(&t, None).is_err());
    }

    #[test]
    fn image_pull_string_prefers_tag_then_digest() {
        let tagged = ImageDetail {
            repository_name: "api".into(),
            digest: Some("sha256:abc".into()),
            tags: vec!["v1.0".into()],
            size_bytes: None,
            pushed_at: None,
            manifest_media_type: None,
            artifact_media_type: None,
            scan_summary: None,
            scan_status: None,
        };
        assert_eq!(image_pull_string(&tagged), "api:v1.0");
        let untagged = ImageDetail {
            repository_name: "api".into(),
            digest: Some("sha256:abc".into()),
            tags: vec![],
            size_bytes: None,
            pushed_at: None,
            manifest_media_type: None,
            artifact_media_type: None,
            scan_summary: None,
            scan_status: None,
        };
        assert_eq!(image_pull_string(&untagged), "api@sha256:abc");
    }
}
