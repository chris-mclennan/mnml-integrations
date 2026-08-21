mod app;
mod codebuild;
mod config;
mod install;
mod keys;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-aws-codebuild",
    version,
    about = "AWS CodeBuild project browser for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/<id>.toml.
    #[arg(long)]
    install: bool,
    /// Remove the mnml integration manifest for this sibling.
    #[arg(long)]
    uninstall: bool,
    /// #1103 f/u7 (2026-08-20) — dump a diagnostic report to stdout
    /// (auth source, config summary, runtime info) and exit.
    #[arg(long)]
    diag: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }

    let cfg = config::load()?;

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!("region: {:?}", cfg.region);
        println!("recent_builds: {}", cfg.recent_builds);
        if cfg.projects.is_empty() {
            println!("projects allow-list: (empty — show every project)");
        } else {
            println!("projects allow-list:");
            for p in &cfg.projects {
                println!("  - {p}");
            }
        }
        println!("(auth: defers to the `aws` CLI's own credential chain)");
        return Ok(());
    }

    if cli.diag {
        println!("mnml-aws-codebuild · diagnostics");
        println!();
        println!("Auth");
        println!("  \u{2514}\u{2500} (run `--check` for full auth resolution details)");
        println!();
        println!("Config");
        println!("  \u{2514}\u{2500} path: {}", config::config_path().display());
        println!();
        println!("Runtime");
        println!("  \u{251c}\u{2500} integration: {}", env!("CARGO_PKG_VERSION"));
        println!(
            "  \u{2514}\u{2500} os/arch: {} / {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return Ok(());
    }

    let mut app = app::App::new(cfg);
    ui::run(&mut app)
}
