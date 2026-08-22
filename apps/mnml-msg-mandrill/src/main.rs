mod app;
mod clipboard;
mod config;
mod install;
mod keys;
mod mandrill;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-mandrill",
    version,
    about = "Mandrill (Mailchimp Transactional) browser for mnml"
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
        let auth = mandrill::Auth::from_env();

        println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        println!("config: {}", config::config_path().display());
        match &cfg {
            Ok(cfg) => {
                println!(
                    "refresh_interval_secs={} · messages_lookback_days={}",
                    cfg.refresh_interval_secs, cfg.messages_lookback_days
                );
                println!("tabs:");
                for (i, t) in cfg.tabs.iter().enumerate() {
                    println!("  {} ({}): kind={}", i + 1, t.name, t.kind);
                }
            }
            Err(e) => println!("config: ERROR — {e}"),
        }

        println!();
        println!("env: MANDRILL_API_KEY={}", mask_env("MANDRILL_API_KEY"));

        match &auth {
            Ok(a) => {
                println!();
                println!("api base: {}", a.api_base());
                print!("auth: ");
                // /users/ping.json is the canonical liveness check.
                match mandrill::ping(a) {
                    Ok(body) => {
                        let trimmed = body.trim_matches('"').trim();
                        println!("ok ({trimmed})");
                    }
                    Err(e) => {
                        println!("ERROR — {e}");
                        std::process::exit(2);
                    }
                }
            }
            Err(e) => {
                println!();
                println!("auth: ERROR — {e}");
                std::process::exit(2);
            }
        }
        if cfg.is_err() {
            std::process::exit(2);
        }
        return Ok(());
    }

    let cfg = config::load()?;
    let auth = match mandrill::Auth::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("setup:");
            eprintln!(
                "  export MANDRILL_API_KEY=...     (from Mandrill: Settings → SMTP & API Info → API Keys)"
            );
            eprintln!();
            eprintln!("then re-run, or `mnml-msg-mandrill --check` to confirm.");
            std::process::exit(2);
        }
    };

    if cli.diag {
        println!("mnml-msg-mandrill · diagnostics");
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
    // 2026-06-08 sibling-sweep fix: dropped the `ends …XXXX` tail.
    // Mandrill keys are 22 chars; leaking 4 reveals ~18% of the
    // entropy. Low real exposure, easy fix. Also fixes a latent
    // multi-byte slice panic on `&v[v.len()-4..]`. Just report length.
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => format!("set ({} chars)", v.len()),
        _ => "(unset)".into(),
    }
}
