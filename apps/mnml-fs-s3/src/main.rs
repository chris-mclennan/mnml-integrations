mod app;
mod bridge_client;
mod clipboard;
mod config;
mod keys;
mod install;
mod s3;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-fs-s3",
    version,
    about = "Amazon S3 browser for mnml — list, drill, download, yank"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Bypass the config file: open a single ad-hoc bucket tab
    /// instead. Pairs with `--prefix` (subtree) and `--region`.
    /// Used by mnml + sibling-handoffs (e.g. cloud-agents row
    /// → "Open S3 artifacts in mnml" passes the qwe-run's
    /// artifact prefix).
    #[arg(long, value_name = "BUCKET")]
    bucket: Option<String>,
    /// Optional starting prefix when `--bucket` is supplied
    /// (`2026/06/` jumps straight into that subtree). Ignored
    /// without `--bucket`.
    #[arg(long, value_name = "PREFIX")]
    prefix: Option<String>,
    /// Optional region override when `--bucket` is supplied
    /// (defaults to the AWS CLI's resolved region).
    #[arg(long, value_name = "REGION")]
    region: Option<String>,
    /// Optional tab label when `--bucket` is supplied. Defaults
    /// to the bucket name.
    #[arg(long, value_name = "NAME")]
    bucket_name: Option<String>,
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


    // `--bucket` bypasses the user's config — synthesize a single-
    // tab config from the CLI args. Mirrors mnml-aws-cloudwatch-logs's
    // `--log-group` cross-sibling-handoff shape.
    let cfg = if let Some(bucket) = cli.bucket.clone() {
        config::Config {
            refresh_interval_secs: 0,
            buckets: vec![config::Bucket {
                name: cli.bucket_name.clone().unwrap_or_else(|| bucket.clone()),
                bucket,
                prefix: cli.prefix.clone(),
                region: cli.region.clone(),
            }],
        }
    } else {
        config::load()?
    };

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        for (i, b) in cfg.buckets.iter().enumerate() {
            println!(
                "  bucket {} ({}): s3://{}{}",
                i + 1,
                b.name,
                b.bucket,
                b.prefix
                    .as_deref()
                    .map(|p| format!("/{p}"))
                    .unwrap_or_default()
            );
        }
        // Sanity-check that the AWS CLI is on PATH.
        match std::process::Command::new("aws").arg("--version").output() {
            Ok(out) if out.status.success() => {
                let v = String::from_utf8_lossy(&out.stdout);
                println!("aws CLI: ok — {}", v.trim());
            }
            Ok(_) => println!("aws CLI: FAIL — `aws --version` exited non-zero"),
            Err(e) => println!("aws CLI: NOT FOUND — {e}"),
        }
        return Ok(());
    }

    let bucket_count = cfg.buckets.len();
    let mut app = app::App::new(cfg)?;

    if bridge_client::is_hosted() {
        bridge_client::toast(&format!(
            "mnml-fs-s3 connected · {bucket_count} bucket(s)"
        ));
    }

    ui::run(&mut app).await
}
