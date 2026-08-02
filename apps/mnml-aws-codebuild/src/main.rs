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

    let mut app = app::App::new(cfg);
    ui::run(&mut app)
}
