mod app;
mod clipboard;
mod config;
mod install;
mod keys;
mod slack;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-slack",
    version,
    about = "Slack browse + post terminal client for the mnml family"
)]
struct Cli {
    /// Print resolved config + auth state (mask token, hit auth.test) and exit.
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
    /// Print every channel your token can see (id + name +
    /// membership) and exit. Use the output to populate
    /// `[channels].show` / `[channels].hide` in the config.
    #[arg(long)]
    list_channels: bool,
    /// Restrict the sibling to one family — `channels` or
    /// `canvases`. Passed by the split rail chips (Slack Channels
    /// vs. Slack Canvases). Omitted → full multi-tab UI.
    #[arg(long)]
    only: Option<String>,
    /// #1044 (2026-08-19). Emit a JSON blob mnml's statusline
    /// segment poller can render as
    /// `{mentions}({dms}) {channels}ch · {presence}` (or any other
    /// format the user configures via `[[statusline_segments]]`).
    /// Runs a bounded 10s fetch; a hung network never freezes the
    /// polling worker.
    #[arg(long)]
    values: bool,
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
    if cli.list_channels {
        let auth = slack::Auth::from_env()?;
        let chans = slack::conversations_list(&auth, "public_channel,private_channel")?;
        let mut chans = chans;
        chans.sort_by_key(|c| c.name.to_lowercase());
        for c in &chans {
            let mark = if c.is_member { "*" } else { " " };
            println!("{mark} #{name}  ({id})", name = c.name, id = c.id);
        }
        println!();
        println!(
            "{} channels ({}* = member)",
            chans.len(),
            chans.iter().filter(|c| c.is_member).count()
        );
        return Ok(());
    }

    if cli.values {
        // #1044 (2026-08-19) — mnml statusline segment. Emits the
        // JSON shape `UnreadSummary` serializes to; see the
        // `[[statusline_segments]]` block in `install.rs` for the
        // default format string. Bounded 10s to survive the polling
        // worker's shorter interval (60s+).
        let auth = slack::Auth::from_env()?;
        // std threads because the whole sibling is sync — reqwest
        // blocking client under the hood.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(slack::fetch_unread_summary(&auth));
        });
        let summary = match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(r) => r?,
            Err(_) => anyhow::bail!("--values timed out after 10s"),
        };
        println!("{}", serde_json::to_string(&summary)?);
        return Ok(());
    }
    if cli.check {
        let cfg = config::load();
        let auth = slack::Auth::from_env();

        println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        println!("config: {}", config::config_path().display());
        match &cfg {
            Ok(cfg) => {
                println!(
                    "tabs ({}, refresh={}s, post_multiline={}):",
                    cfg.tabs.len(),
                    cfg.refresh_interval_secs,
                    cfg.post_multiline
                );
                for (i, t) in cfg.tabs.iter().enumerate() {
                    println!("  {} ({}): kind={}", i + 1, t.name, t.kind);
                }
            }
            Err(e) => println!("config: ERROR — {e}"),
        }

        println!();
        println!("env: SLACK_USER_TOKEN={}", mask_env("SLACK_USER_TOKEN"));
        println!("env: SLACK_BOT_TOKEN={}", mask_env("SLACK_BOT_TOKEN"));

        match &auth {
            Ok(a) => {
                println!();
                println!("api base: {}", a.api_base());
                println!("token kind: {} ({})", a.kind, slack::mask_token(&a.token));
                match slack::auth_test(a) {
                    Ok(test) => {
                        println!();
                        println!("auth.test: ok");
                        println!("  team:    {} ({})", test.team, test.team_id);
                        println!("  user:    {} ({})", test.user, test.user_id);
                        if !test.url.is_empty() {
                            println!("  url:     {}", test.url);
                        }
                    }
                    Err(e) => {
                        println!();
                        println!("auth.test: ERROR — {e}");
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

    let mut cfg = config::load()?;
    // 2026-07-22 — `--only <family>` narrows the tab list per the
    // split rail chips. `channels` keeps just the `channels` tab;
    // `canvases` opens with a stub `canvases` tab (v0.1 — full
    // `files.list?type=canvas` rendering is a follow-up).
    if let Some(only) = cli.only.as_deref() {
        match only {
            "channels" => {
                cfg.tabs
                    .retain(|t| t.kind == "channels" || t.kind == "search");
                if cfg.tabs.is_empty() {
                    cfg.tabs.push(config::Tab {
                        name: "channels".into(),
                        kind: "channels".into(),
                        query: None,
                    });
                }
            }
            "canvases" => {
                cfg.tabs = vec![config::Tab {
                    name: "canvases".into(),
                    kind: "canvases".into(),
                    query: None,
                }];
            }
            other => {
                eprintln!("error: --only expects `channels` or `canvases`, got `{other}`");
                std::process::exit(2);
            }
        }
    }
    let auth = match slack::Auth::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("setup:");
            eprintln!("  1. visit https://api.slack.com/apps and create an app");
            eprintln!("  2. add the User-token scopes (see README)");
            eprintln!("  3. install the app to your workspace");
            eprintln!("  4. copy the User OAuth Token (xoxp-…)");
            eprintln!("  5. export SLACK_USER_TOKEN=xoxp-...");
            eprintln!();
            eprintln!("then re-run, or `mnml-msg-slack --check` to confirm.");
            std::process::exit(2);
        }
    };

    // Pane tab title — distinguishes the two split-rail entry modes in
    // mnml's bufferline (`--only channels` vs `--only canvases`); the
    // unrestricted full multi-tab UI keeps the plain "Slack" title.
    let title = match cli.only.as_deref() {
        Some("channels") => "Slack Channels",
        Some("canvases") => "Slack Canvases",
        _ => "Slack",
    };

    let mut app = app::App::new(cfg, auth)?;
    ui::run(&mut app, title)
}

fn mask_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => slack::mask_token(&v),
        _ => "(unset)".into(),
    }
}
