//! `mnml-meta-stats` — download-stats report for the mnml crate family.
//!
//! One-shot: fetches crates.io + GitHub Releases APIs, renders a
//! human-readable table + 30-day sparklines, exits. Meant to run as
//! a Pty pane inside mnml (or straight from a shell), not as an
//! interactive TUI — refresh = rerun.
//!
//! All upstream data is public: crates.io download counts are
//! visible at `crates.io/crates/<name>`, GitHub release assets at
//! `github.com/chris-mclennan/mnml/releases`. No auth needed for
//! either API.

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;

mod install;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-meta-stats",
    version,
    about = "Download stats for the mnml crate family"
)]
struct Cli {
    /// Register with mnml (writes an integration manifest so a rail
    /// chip + palette command show up on next mnml start).
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest.
    #[arg(long)]
    uninstall: bool,
    /// Emit the report as JSON on stdout instead of the pretty table.
    /// For scripting.
    #[arg(long)]
    json: bool,
    /// crates.io keywords to search (comma-separated). Family
    /// crates aren't perfectly consistent — most integrations tag
    /// `mnml-integration`, `mnml-bridge` itself only tags `mnml`.
    /// Results across keywords are deduped by crate name.
    #[arg(long, default_value = "mnml-integration,mnml")]
    keywords: String,
    /// GitHub repo (owner/name) to pull release assets from. The
    /// mnml binary itself is here; other family repos can be added
    /// later.
    #[arg(long, default_value = "chris-mclennan/mnml")]
    gh_repo: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }
    let report = fetch_report(&cli.keywords, &cli.gh_repo)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

// ── Data shapes ─────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
struct Report {
    crates: Vec<CrateStat>,
    releases: Vec<ReleaseAsset>,
    totals: Totals,
}

#[derive(Debug, serde::Serialize)]
struct Totals {
    crate_count: usize,
    crate_total_downloads: u64,
    crate_recent_downloads: u64,
    release_asset_count: usize,
    release_asset_downloads: u64,
}

#[derive(Debug, serde::Serialize)]
struct CrateStat {
    name: String,
    total_downloads: u64,
    recent_downloads: u64,
    latest_version: String,
    daily_last_30: Vec<u64>,
}

#[derive(Debug, serde::Serialize)]
struct ReleaseAsset {
    tag: String,
    asset: String,
    downloads: u64,
    size_mb: f64,
}

// ── Fetch ───────────────────────────────────────────────────────────

const USER_AGENT: &str = concat!(
    "mnml-meta-stats/",
    env!("CARGO_PKG_VERSION"),
    " (chris-mclennan/mnml)"
);

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("build http client")
}

fn fetch_report(keywords_csv: &str, gh_repo: &str) -> Result<Report> {
    let c = client()?;
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for kw in keywords_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match fetch_crate_names(&c, kw) {
            Ok(ns) => names.extend(ns),
            Err(e) => eprintln!("keyword {kw:?} search failed: {e:#}"),
        }
    }
    let mut crates: Vec<CrateStat> = Vec::with_capacity(names.len());
    for name in &names {
        match fetch_crate_stat(&c, name) {
            Ok(s) => crates.push(s),
            Err(e) => eprintln!("skipping {name}: {e:#}"),
        }
    }
    crates.sort_by(|a, b| b.total_downloads.cmp(&a.total_downloads));

    let releases = fetch_release_assets(&c, gh_repo).unwrap_or_else(|e| {
        eprintln!("release fetch failed for {gh_repo}: {e:#}");
        Vec::new()
    });

    let totals = Totals {
        crate_count: crates.len(),
        crate_total_downloads: crates.iter().map(|c| c.total_downloads).sum(),
        crate_recent_downloads: crates.iter().map(|c| c.recent_downloads).sum(),
        release_asset_count: releases.len(),
        release_asset_downloads: releases.iter().map(|r| r.downloads).sum(),
    };
    Ok(Report {
        crates,
        releases,
        totals,
    })
}

#[derive(Deserialize)]
struct CratesSearchResp {
    crates: Vec<CratesSearchEntry>,
    meta: CratesSearchMeta,
}
#[derive(Deserialize)]
struct CratesSearchMeta {
    next_page: Option<String>,
}
#[derive(Deserialize)]
struct CratesSearchEntry {
    id: String,
}

fn fetch_crate_names(c: &reqwest::blocking::Client, keyword: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut url =
        format!("https://crates.io/api/v1/crates?keyword={keyword}&per_page=100&sort=alpha");
    loop {
        let resp: CratesSearchResp = c.get(&url).send()?.error_for_status()?.json()?;
        for entry in resp.crates {
            out.push(entry.id);
        }
        match resp.meta.next_page {
            Some(next) => url = format!("https://crates.io/api/v1{next}"),
            None => break,
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct CrateDetailResp {
    #[serde(rename = "crate")]
    krate: CrateDetail,
}
#[derive(Deserialize)]
struct CrateDetail {
    downloads: u64,
    recent_downloads: Option<u64>,
    max_stable_version: Option<String>,
    max_version: String,
}

#[derive(Deserialize)]
struct DownloadsResp {
    version_downloads: Vec<VersionDownload>,
}
#[derive(Deserialize)]
struct VersionDownload {
    date: String,
    downloads: u64,
}

fn fetch_crate_stat(c: &reqwest::blocking::Client, name: &str) -> Result<CrateStat> {
    let detail: CrateDetailResp = c
        .get(format!("https://crates.io/api/v1/crates/{name}"))
        .send()?
        .error_for_status()?
        .json()?;
    let downloads: DownloadsResp = c
        .get(format!("https://crates.io/api/v1/crates/{name}/downloads"))
        .send()?
        .error_for_status()?
        .json()?;
    let daily_last_30 = last_30_days(&downloads.version_downloads);
    Ok(CrateStat {
        name: name.to_string(),
        total_downloads: detail.krate.downloads,
        recent_downloads: detail.krate.recent_downloads.unwrap_or(0),
        latest_version: detail
            .krate
            .max_stable_version
            .unwrap_or(detail.krate.max_version),
        daily_last_30,
    })
}

fn last_30_days(rows: &[VersionDownload]) -> Vec<u64> {
    let mut by_date: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for r in rows {
        *by_date.entry(r.date.as_str()).or_insert(0) += r.downloads;
    }
    let mut recent: Vec<u64> = by_date.into_iter().rev().take(30).map(|(_, v)| v).collect();
    recent.reverse();
    while recent.len() < 30 {
        recent.insert(0, 0);
    }
    recent
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}
#[derive(Deserialize)]
struct GhAsset {
    name: String,
    download_count: u64,
    size: u64,
}

fn fetch_release_assets(c: &reqwest::blocking::Client, repo: &str) -> Result<Vec<ReleaseAsset>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
    let releases: Vec<GhRelease> = c
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json()?;
    let mut out = Vec::new();
    for r in releases {
        for a in r.assets {
            // Skip checksum + manifest sidecars — they're always
            // downloaded 1:1 with the real artifact and just double
            // the row count.
            if a.name.ends_with(".sha256")
                || a.name.ends_with(".sum")
                || a.name == "dist-manifest.json"
            {
                continue;
            }
            out.push(ReleaseAsset {
                tag: r.tag_name.clone(),
                asset: a.name,
                downloads: a.download_count,
                size_mb: (a.size as f64) / 1_048_576.0,
            });
        }
    }
    out.sort_by(|a, b| b.downloads.cmp(&a.downloads));
    Ok(out)
}

// ── Render ──────────────────────────────────────────────────────────

const SPARK_BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn sparkline(values: &[u64]) -> String {
    let max = values.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return "─".repeat(values.len());
    }
    values
        .iter()
        .map(|v| {
            if *v == 0 {
                ' '
            } else {
                let idx =
                    ((*v as f64 / max as f64) * (SPARK_BARS.len() - 1) as f64).round() as usize;
                SPARK_BARS[idx.min(SPARK_BARS.len() - 1)]
            }
        })
        .collect()
}

fn print_report(r: &Report) {
    println!("mnml family — crates.io downloads");
    println!("─────────────────────────────────────────────────────────────────────────");
    println!(
        "{:<28}  {:>8}  {:>9}  {:<10}  {:<32}",
        "crate", "total", "last-90d", "version", "30d ▁▂▃▄▅▆▇█"
    );
    println!("─────────────────────────────────────────────────────────────────────────");
    for c in &r.crates {
        println!(
            "{:<28}  {:>8}  {:>9}  {:<10}  {}",
            c.name,
            c.total_downloads,
            c.recent_downloads,
            c.latest_version,
            sparkline(&c.daily_last_30)
        );
    }
    println!("─────────────────────────────────────────────────────────────────────────");
    println!(
        "totals  crates={}  total={}  last-90d={}",
        r.totals.crate_count, r.totals.crate_total_downloads, r.totals.crate_recent_downloads
    );

    if !r.releases.is_empty() {
        println!();
        println!("chris-mclennan/mnml — GitHub release assets");
        println!("─────────────────────────────────────────────────────────────────────────");
        println!("{:<30}  {:>8}  {:>8}  {}", "tag", "dls", "size MB", "asset");
        println!("─────────────────────────────────────────────────────────────────────────");
        for a in &r.releases {
            println!(
                "{:<30}  {:>8}  {:>8.1}  {}",
                a.tag, a.downloads, a.size_mb, a.asset
            );
        }
        println!("─────────────────────────────────────────────────────────────────────────");
        println!(
            "totals  assets={}  downloads={}",
            r.totals.release_asset_count, r.totals.release_asset_downloads
        );
    }
}
