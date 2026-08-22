mod app;
mod clipboard;
mod config;
mod datadog;
mod install;
mod keys;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-obs-datadog",
    version,
    about = "Datadog observability browser for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
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
    /// #1103 f/u7 (2026-08-20) — dump a diagnostic report to stdout
    /// (auth source, config summary, runtime info) and exit.
    #[arg(long)]
    diag: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // --install / --uninstall run before auth / config so the
    // first-run install doesn't require credentials to be set up.
    if cli.install {
        return install::install();
    }
    if cli.uninstall {
        return install::uninstall();
    }

    if cli.check {
        let cfg = config::load();
        let auth = datadog::Auth::from_env();

        println!("config: {}", config::config_path().display());
        match &cfg {
            Ok(cfg) => {
                println!("tabs:");
                for (i, t) in cfg.tabs.iter().enumerate() {
                    println!(
                        "  {} ({}): kind={} query={:?}",
                        i + 1,
                        t.name,
                        t.kind,
                        t.query
                    );
                }
            }
            Err(e) => println!("config: ERROR — {e}"),
        }

        println!();
        println!("env: DD_API_KEY={}", mask_env("DD_API_KEY"));
        println!("env: DD_APP_KEY={}", mask_env("DD_APP_KEY"));
        println!(
            "env: DD_SITE={}",
            std::env::var("DD_SITE").unwrap_or_else(|_| "(unset → datadoghq.com)".into())
        );

        match &auth {
            Ok(a) => {
                println!();
                println!("api base v1: {}", a.api_base_v1());
                println!("api base v2: {}", a.api_base_v2());
                println!("app base:    {}", a.app_base());
                println!("auth: ok");
            }
            Err(e) => {
                println!();
                println!("auth: ERROR — {e}");
                std::process::exit(2);
            }
        }
        // If config errored, still exit non-zero so callers can
        // chain `&&` safely.
        if cfg.is_err() {
            std::process::exit(2);
        }
        return Ok(());
    }

    let cfg = config::load()?;
    let auth = match datadog::Auth::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("setup:");
            eprintln!("  export DD_API_KEY=...       (from Datadog: Org Settings → API Keys)");
            eprintln!(
                "  export DD_APP_KEY=...       (from Datadog: Org Settings → Application Keys)"
            );
            eprintln!("  export DD_SITE=datadoghq.com   (defaults to the US1 site)");
            eprintln!();
            eprintln!("then re-run, or `mnml-obs-datadog --check` to confirm.");
            std::process::exit(2);
        }
    };

    if cli.diag {
        println!("mnml-obs-datadog · diagnostics");
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

    let mut app = app::App::new(cfg, auth)?;
    ui::run(&mut app)
}

fn mask_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => {
            if v.len() > 6 {
                format!("set ({} chars, ends …{})", v.len(), &v[v.len() - 4..])
            } else {
                format!("set ({} chars)", v.len())
            }
        }
        _ => "(unset)".into(),
    }
}
