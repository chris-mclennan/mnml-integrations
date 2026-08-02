mod app;
mod buttondown;
mod clipboard;
mod config;
mod keys;
mod theme;
mod ui;
mod install;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-buttondown",
    version,
    about = "Buttondown newsletter browser for mnml"
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
        let auth = buttondown::Auth::from_env();

        println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        println!("config: {}", config::config_path().display());
        match &cfg {
            Ok(cfg) => {
                println!("tabs:");
                for (i, t) in cfg.tabs.iter().enumerate() {
                    println!("  {} ({}): kind={}", i + 1, t.name, t.kind);
                }
            }
            Err(e) => println!("config: ERROR — {e}"),
        }

        println!();
        println!("env: BUTTONDOWN_API_KEY={}", mask_env("BUTTONDOWN_API_KEY"));

        match &auth {
            Ok(a) => {
                println!();
                println!("api base: {}", a.api_base());
                println!("web base: {}", a.web_base());
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
    let auth = match buttondown::Auth::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("setup:");
            eprintln!(
                "  export BUTTONDOWN_API_KEY=...   (from Buttondown: Settings → Programming)"
            );
            eprintln!();
            eprintln!("then re-run, or `mnml-msg-buttondown --check` to confirm.");
            std::process::exit(2);
        }
    };

    let mut app = app::App::new(cfg, auth)?;
    ui::run(&mut app)
}

fn mask_env(name: &str) -> String {
    // 2026-06-08 sibling-sweep fix: dropped the `ends …XXXX` tail
    // (leaked ~20% of the entropy of short keys) and the latent
    // multi-byte slice panic on `&v[v.len()-4..]`. Just report length.
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => format!("set ({} chars)", v.len()),
        _ => "(unset)".into(),
    }
}
