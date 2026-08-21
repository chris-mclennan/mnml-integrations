mod app;
mod clipboard;
mod cloudflare;
mod config;
mod install;
mod keys;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-cdn-cloudflare",
    version,
    about = "Cloudflare CDN browser for mnml"
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
        let auth = cloudflare::Auth::from_env();

        println!("config: {}", config::config_path().display());
        match &cfg {
            Ok(cfg) => {
                println!("tabs:");
                for (i, t) in cfg.tabs.iter().enumerate() {
                    let zone = t
                        .zone_id
                        .as_deref()
                        .map(|z| format!(" zone_id={z}"))
                        .unwrap_or_default();
                    println!("  {} ({}): kind={}{}", i + 1, t.name, t.kind, zone);
                }
            }
            Err(e) => println!("config: ERROR — {e}"),
        }

        println!();
        println!(
            "env: CLOUDFLARE_API_TOKEN={}",
            mask_env("CLOUDFLARE_API_TOKEN")
        );
        println!(
            "env: CLOUDFLARE_ACCOUNT_ID={}",
            std::env::var("CLOUDFLARE_ACCOUNT_ID").unwrap_or_else(|_| "(unset)".into())
        );

        match &auth {
            Ok(a) => {
                println!();
                println!("api base:    {}", cloudflare::API_BASE);
                match cloudflare::verify_token(a) {
                    Ok(tv) => {
                        println!("token id:    {}", tv.id);
                        println!("token status: {}", tv.status);
                        // Best-effort email lookup. Tokens without
                        // `User Details:Read` will 403 — that's fine,
                        // verify already passed.
                        match cloudflare::user_info(a) {
                            Ok(u) => {
                                println!(
                                    "token email: {}",
                                    u.email.as_deref().unwrap_or("(not returned)")
                                );
                            }
                            Err(_) => println!("token email: (scope missing — User Details:Read)"),
                        }
                        println!("auth: ok");
                    }
                    Err(e) => {
                        println!("auth: ERROR — {e}");
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
    let auth = match cloudflare::Auth::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("setup:");
            eprintln!(
                "  export CLOUDFLARE_API_TOKEN=...     (dash.cloudflare.com → My Profile → API Tokens)"
            );
            eprintln!(
                "  export CLOUDFLARE_ACCOUNT_ID=...    (dash.cloudflare.com sidebar → Account ID — required for workers / pages tabs)"
            );
            eprintln!();
            eprintln!("then re-run, or `mnml-cdn-cloudflare --check` to confirm.");
            std::process::exit(2);
        }
    };

    if cli.diag {
        println!("mnml-cdn-cloudflare · diagnostics");
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
