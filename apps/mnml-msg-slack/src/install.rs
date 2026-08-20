//! `--install` / `--uninstall` subcommand — writes integration
//! manifests at `~/.config/mnml/integrations/<id>.toml` so mnml
//! picks up the rail chips + palette commands + chord bindings on
//! next startup.
//!
//! 2026-07-22 — split the single `slack` chip into TWO family
//! chips (mirroring the Bitbucket PRs / Pipelines split):
//!
//!   - `slack_channels`   — `mnml-msg-slack --only channels`
//!   - `slack_canvases`     — `mnml-msg-slack --only canvases`
//!
//! 2026-08-09 (0.1.2) — three fixes driven by mnml's sibling glyph
//! stability audit (`scratchpad/sibling-audit-2026-08-09.md`) +
//! user morning direction:
//!   * Glyph swap: F117F (channels) + F0F6 (canvases) → F07D2
//!     mdi-slack on BOTH. F117F was accidentally routing to a random
//!     Symbols Nerd Font Mono glyph; F0F6 sat OUTSIDE ghostty's
//!     `font-codepoint-map` ranges entirely → guaranteed tofu.
//!
//! 2026-08-09 (0.1.3) — F07D2 was ALSO wrong: on current Nerd Font
//! versions that codepoint renders as an unrelated (house-shaped)
//! icon, not slack. Bumped to F03EF (also in the routed
//! F0001-F1AFF range) which is the codepoint mnml's own
//! `src/icon_catalog.rs` uses for slack and which renders as the
//! Slack logo on the Nerd Font builds the audit tested.
//!   * Colors: channels → `white`, canvases → `yellow`. Distinguishes
//!     the two chips when the glyph is identical.
//!   * `slack_canvases` → `slack_boards` (id + label). Added to the
//!     PREDECESSOR_IDS uninstall cleanup so existing users get the
//!     old `slack_canvases.toml` manifest removed on next `--install`.
//!
//! 2026-08-19 (0.1.4, #1063) — REVERT: `slack_boards` → `slack_canvases`
//! (id + label). The 0.1.3 comment claimed the rename matched "Slack's
//! own product-marketing name" but Slack's actual product name for the
//! feature is Canvas / Canvases, not Boards. The board label was
//! confusing users during the mnml hero-demo QA. `slack_boards` is
//! added to PREDECESSOR_IDS so the round-trip cleans up the interim
//! manifest name.
//!
//! `PREDECESSOR_IDS` uninstalls run BEFORE the new manifest writes.

use anyhow::Result;
use mnml_bridge::{
    AuthField, ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const CHANNELS_ID: &str = "slack_channels";
const CANVASES_ID: &str = "slack_canvases";

/// Ids of PRIOR manifest names this sibling has written but no
/// longer wants. Every entry is unconditionally uninstalled on each
/// `--install` so an upgrade doesn't leave orphan chips in the rail.
///
/// - `slack` (pre-0.1) — the single-chip form before the
///   channels/canvases split (2026-07-22).
/// - `slack_boards` (0.1.2–0.1.3) — the misnamed interim id before
///   `slack_canvases` was restored in 0.1.4 (#1063). No entry for
///   `slack_canvases` itself because that IS the current id.
const PREDECESSOR_IDS: &[&str] = &["slack", "slack_boards"];

/// Shared auth-field schema for both `slack_channels` + `slack_canvases`.
/// Both chips run the same binary + hit the same Slack Web API, so
/// they need the same token; using one shared `auth_fields()` helper
/// makes that explicit + writes identical `[[auth]]` blocks to both
/// manifests. mnml's per-integration Settings pane reads these
/// declarations + renders a form; save writes user answers under
/// `[auth_values]` in the same TOML.
///
/// Added in 0.1.3 (2026-08-11) — requires `mnml-bridge = "0.7"`.
fn auth_fields() -> Vec<AuthField> {
    vec![
        AuthField {
            key: "bot_token".into(),
            label: "Slack bot token".into(),
            kind: "secret".into(),
            env_fallback: Some("SLACK_BOT_TOKEN".into()),
            help_url: Some("https://api.slack.com/apps".into()),
            help: Some(
                "Create a Slack app + install to workspace + copy the Bot User OAuth Token."
                    .into(),
            ),
            required: true,
        },
        AuthField {
            key: "team_id".into(),
            label: "Team ID (optional)".into(),
            kind: "text".into(),
            env_fallback: None,
            help_url: None,
            help: Some(
                "Optional. Restricts channel/board listing to this workspace. Blank = all workspaces the token can see."
                    .into(),
            ),
            required: false,
        },
    ]
}

pub fn install() -> Result<()> {
    // Drop every legacy manifest first so we don't end up with
    // 3+ chips after upgrading.
    for pid in PREDECESSOR_IDS {
        let _ = uninstall_integration(pid);
    }

    let channels = IntegrationSpec {
        id: CHANNELS_ID.into(),
        label: "Slack Channels".into(),
        description: Some("Slack channels + DMs + threads + search + post".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-slack".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            // F03EF = slack in mnml's `src/icon_catalog.rs`.
            // Ghostty's font-codepoint-map routes F0001-F1AFF to
            // `Symbols Nerd Font Mono`, so this codepoint renders
            // as the canonical Slack logo. (v0.1.2 shipped F07D2
            // on the assumption it was mdi-slack; on current
            // Nerd Font versions F07D2 renders as an unrelated
            // house-shaped glyph.)
            glyph: "\u{F04B1}".into(),
            fallback: "Sk".into(),
            color: "white".into(),
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(CHANNELS_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "slack.open_channels".into(),
            title: "Slack: open channels".into(),
            group: Some("integrations".into()),
            keys: vec!["<leader>iS".into()],
            run: ":term mnml-msg-slack --only channels".into(),
        }],
        auth: auth_fields(),
        ..Default::default()
    };
    let path = install_integration(&channels)?;
    // #1044 (2026-08-19) — statusline segment declaration. Same
    // raw-TOML append pattern the Bitbucket PRs manifest uses
    // (bridge 0.7 doesn't serialize `[[values_sources]]` /
    // `[[statusline_segments]]` from typed structs yet; bridge 0.8+
    // will, and this helper can be deleted then). Only the
    // Channels chip carries the segment — Boards has no ambient
    // count worth surfacing yet.
    if let Err(e) = append_segment_blocks(CHANNELS_ID, &path) {
        eprintln!(
            "note: couldn't append [[values_sources]] to {} ({e}) — hand-edit that file to add the chip",
            path.display()
        );
    }
    println!("wrote manifest: {}", path.display());

    let boards = IntegrationSpec {
        id: CANVASES_ID.into(),
        label: "Slack Canvases".into(),
        description: Some("Slack Canvases — canvases from files.list, open in browser".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        binary: "mnml-msg-slack".into(),
        category: Some("msg".into()),
        chip: Some(ChipSpec {
            // Same F03EF slack outline as channels — the CHIP
            // COLOR is what tells the two apart in the rail. Sharing
            // the outline matches the "one bundled crate → many
            // chips, one shared glyph, per-chip color" pattern the
            // stability audit proposes.
            glyph: "\u{F04B1}".into(),
            fallback: "SB".into(),
            color: "yellow".into(),
            // 2026-07-22 — enabled by default so users see BOTH
            // chips after --install. #1005 (2026-08-19): the Boards
            // view now actually renders canvases (title / owner /
            // Enter-to-open-in-browser); the "v0.1 stub" call-out
            // was retired with the marketplace listing.
            enabled: true,
            in_palette_bar: false,
            badge_key: Some(CANVASES_ID.into()),
            ..Default::default()
        }),
        commands: vec![CommandSpec {
            id: "slack.open_canvases".into(),
            title: "Slack: open canvases".into(),
            group: Some("integrations".into()),
            keys: vec![],
            // Argument name stays `canvases` — that's the Slack
            // API surface name. Only the mnml-facing chip label
            // and manifest id changed.
            run: ":term mnml-msg-slack --only canvases".into(),
        }],
        auth: auth_fields(),
        ..Default::default()
    };
    let path = install_integration(&boards)?;
    println!("wrote manifest: {}", path.display());

    println!("run mnml + `integrations.refresh` (or restart) to pick up the rail chips");
    Ok(())
}

/// #1044 (2026-08-19) — append mnml 0.2.11+ statusline-segment
/// TOML blocks to a freshly-written manifest. Idempotent: the
/// segment id is unique enough that re-installing detects it and
/// skips the append. Only the Channels chip declares a segment
/// today. Deletion note: once `mnml-bridge` 0.8+ is on crates.io
/// (which will serialize these blocks from typed structs), this
/// whole helper can be removed and the fields lifted into
/// `IntegrationSpec`.
fn append_segment_blocks(chip_id: &str, path: &std::path::Path) -> std::io::Result<()> {
    if chip_id != CHANNELS_ID {
        return Ok(());
    }
    let current = std::fs::read_to_string(path)?;
    const SEGMENT_ID: &str = "slack_unread";
    if current.contains(SEGMENT_ID) {
        return Ok(());
    }
    // Format:
    //   `<mentions>(<dms>) <channels>ch · <presence>`
    // Chip color hints:
    //   red when mentions > 0
    //   yellow when dm_unread > 0 OR channel_unread_count > 0
    //   dim otherwise
    // (mnml's statusline segment worker evaluates the color hint
    // via the `color = "..."` static + user-configured override in
    // Settings; a "dynamic by count" scheme is a future extension.)
    // Glyph F0BE7 = nf-md-slack in the mnml-routed Symbols Nerd
    // Font Mono range. Falls back to the chip color if not present.
    let block = concat!(
        "\n",
        "# mnml 0.2.11+ statusline segment — appended by\n",
        "# mnml-msg-slack --install. Idempotent: re-install skips\n",
        "# this if `slack_unread` is already present. #1044.\n",
        "[[values_sources]]\n",
        "id = \"slack_values\"\n",
        "command = \"mnml-msg-slack --values\"\n",
        "poll_interval_secs = 120\n",
        "\n",
        "[[statusline_segments]]\n",
        "id = \"slack_unread\"\n",
        "source = \"slack_values\"\n",
        "glyph = \"\u{F04B1}\"\n",
        "color = \"white\"\n",
        "format = \"{mentions}({dm_unread}) {channel_unread_count}ch · {presence}\"\n",
        "tooltip = \"Slack: @mentions, unread DMs, channels with unread, presence. Click to open Slack Channels.\"\n",
        "click_command = \"slack.open_channels\"\n",
    );
    let mut out = current;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    std::fs::write(path, out)
}

pub fn uninstall() -> Result<()> {
    let mut removed_any = false;
    for id in PREDECESSOR_IDS.iter().chain([&CHANNELS_ID, &CANVASES_ID]) {
        if uninstall_integration(id)? {
            println!("removed manifest for {id}");
            removed_any = true;
        }
    }
    if !removed_any {
        println!("no manifests found (already uninstalled)");
    }
    Ok(())
}
