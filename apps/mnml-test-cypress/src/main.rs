mod app;
mod clipboard;
mod cypress;
mod keys;
mod theme;
mod ui;
mod install;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-test-cypress",
    version,
    about = "Cypress test results viewer for mnml — mochawesome JSON"
)]
struct Cli {
    /// Path to a `mochawesome.json` (or to a directory containing
    /// one — looks for `mochawesome.json` / `output.json` /
    /// `results/mochawesome.json`). Not required when only
    /// `--install` / `--uninstall` is used.
    path: Option<PathBuf>,
    /// Print resolved path + parsed stats and exit.
    #[arg(long)]
    check: bool,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/<id>.toml so the
    /// rail chip + palette command + chord appear on the next
    /// mnml startup (or after `integrations.refresh`).
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest for this sibling.
    #[arg(long)]
    uninstall: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // --install / --uninstall run before auth / config so the
    // first-run install doesn't require credentials to be set up.
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }

    let path = cli
        .path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("PATH is required (or use --install / --uninstall)"))?;
    let resolved = resolve_input(path)?;

    let report = cypress::load(&resolved)?;

    if cli.check {
        println!("source: {}", resolved.display());
        let s = &report.stats;
        println!(
            "  tests={} passes={} failures={} pending={} duration={}",
            s.tests,
            s.passes,
            s.failures,
            s.pending,
            cypress::fmt_duration(s.duration_ms)
        );
        println!("  specs: {}", report.specs.len());
        for spec in &report.specs {
            println!(
                "    - {} ({} tests)",
                if spec.full_file.is_empty() {
                    spec.file.clone()
                } else {
                    spec.full_file.clone()
                },
                spec.tests.len()
            );
        }
        return Ok(());
    }

    let mut app = app::App::new(resolved, report)?;

    ui::run(&mut app).await
}

/// Accept a JSON file path directly, or a directory — in which
/// case look for the conventional cypress / mochawesome filenames.
fn resolve_input(input: &std::path::Path) -> Result<PathBuf> {
    if input.is_file() {
        return Ok(input.to_path_buf());
    }
    if input.is_dir() {
        let candidates = [
            "mochawesome.json",
            "output.json",
            "results/mochawesome.json",
        ];
        for c in candidates {
            let p = input.join(c);
            if p.is_file() {
                return Ok(p);
            }
        }
        anyhow::bail!(
            "{} is a directory but no mochawesome.json / output.json / results/mochawesome.json was found inside",
            input.display()
        );
    }
    anyhow::bail!("{} does not exist", input.display())
}
