mod app;
mod clipboard;
mod config;
mod install;
mod keys;
mod log_tail;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-aws-cloudwatch-logs",
    version,
    about = "AWS CloudWatch Logs live tail viewer for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Override the configured tabs with a single one-off tab
    /// tailing this CloudWatch log group. Used by cross-sibling
    /// handoffs (e.g. `mnml-aws-lambda` passes
    /// `--log-group /aws/lambda/<focused-fn>` when the user hits
    /// `l` on a focused function). Pairs with `--log-group-name`
    /// to customise the tab label; defaults to the log group's
    /// final path segment.
    #[arg(long, value_name = "LOG_GROUP")]
    log_group: Option<String>,
    /// Optional human-readable tab name when `--log-group` is
    /// supplied. Defaults to the log group's last path segment.
    #[arg(long, value_name = "NAME")]
    log_group_name: Option<String>,
    /// Optional CloudWatch Logs filter pattern to pair with
    /// `--log-group`. Same syntax as the config's `filter` field.
    #[arg(long, value_name = "PATTERN")]
    filter: Option<String>,
    /// Optional AWS region override when `--log-group` is supplied
    /// (otherwise the config's region wins).
    #[arg(long, value_name = "REGION")]
    region: Option<String>,
    /// Register this sibling with mnml — writes an integration
    /// manifest at ~/.config/mnml/integrations/<id>.toml so the
    /// rail chip + palette command + chord appear on the next
    /// mnml startup (or after `integrations.refresh`).
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

    // `--log-group` bypasses the user's config entirely — it's the
    // cross-sibling handoff path. A one-off tab is synthesized from
    // the CLI args; the on-disk config.toml is left untouched.
    let cfg = if let Some(log_group) = cli.log_group.clone() {
        config::Config::one_off_tab(
            log_group,
            cli.log_group_name.clone(),
            cli.filter.clone(),
            cli.region.clone(),
        )
    } else {
        config::load()?
    };

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!("region: {:?}", cfg.region);
        for (i, t) in cfg.tabs.iter().enumerate() {
            println!(
                "  tab {} ({}): log_group={} log_stream={:?} filter={:?}",
                i + 1,
                t.name,
                t.log_group,
                t.log_stream,
                t.filter
            );
        }
        println!("(auth: defers to the `aws` CLI's own credential chain)");
        return Ok(());
    }

    if cli.diag {
        println!("mnml-aws-cloudwatch-logs · diagnostics");
        println!();
        println!("Auth");
        println!("  \u{2514}\u{2500} (run `--check` for full auth resolution details)");
        println!();
        println!("Config");
        println!(
            "  \u{2514}\u{2500} path: {}",
            config::config_path().display()
        );
        println!();
        println!("Runtime");
        println!(
            "  \u{251c}\u{2500} integration: {}",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "  \u{2514}\u{2500} os/arch: {} / {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return Ok(());
    }

    let mut app = app::App::new(cfg)?;
    ui::run(&mut app).await
}
