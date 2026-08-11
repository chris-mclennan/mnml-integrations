//! `--install` / `--uninstall` subcommands.
//!
//! 2026-07-25 — split into three rail chips to match the bitbucket
//! split pattern. Each chip drops the user straight into a
//! single-purpose view via `--only <family>`:
//!   - Jira Work         → `mnml-tracker-jira --only work`
//!   - Jira Fix Versions → `mnml-tracker-jira --only fix-versions`
//!   - Jira Boards       → `mnml-tracker-jira --only boards`
//!
//! The pre-split combined "jira" manifest gets removed on install
//! so it doesn't duplicate as a fourth chip. `--uninstall` removes
//! all four (three splits + the legacy combined) for symmetry with
//! a user who's been on both schemes over time.

use anyhow::Result;
use mnml_bridge::{
    AuthField, ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const LEGACY_ID: &str = "jira";

/// One rail chip's registration data. `id` doubles as the manifest
/// filename (`~/.config/mnml/integrations/<id>.toml`) and the
/// mnml-side chip id for right-click / config overrides.
struct SplitChip {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    fallback: &'static str,
    label: &'static str,
    color: &'static str,
    only_flag: &'static str,
    /// Leader chord for the palette command. Kept distinct per
    /// chip so users can wire quick access to any of the three.
    leader_keys: &'static str,
    command_id: &'static str,
    command_title: &'static str,
}

/// The three split chips. Same glyph (nf-md-jira F0303 — matches
/// what mnml core's marketplace catalog uses for the tracker entry)
/// across all three — the tooltip + fallback letters distinguish them.
/// Colors: work=blue (mine), fix-versions=green (releases),
/// boards=magenta (planning) — matches the roles.
const SPLITS: &[SplitChip] = &[
    SplitChip {
        id: "jira_work",
        name: "Jira Work",
        description: "Jira: tickets assigned to me + recently done",
        fallback: "JW",
        label: "Jira Work",
        color: "blue",
        only_flag: "work",
        leader_keys: "<leader>ijw",
        command_id: "jira_work.open",
        command_title: "Jira Work: open",
    },
    SplitChip {
        id: "jira_fix_versions",
        name: "Jira Fix Versions",
        description: "Jira: current release grouped by status, with linked PRs + pipelines",
        fallback: "JV",
        label: "Jira Fix Versions",
        color: "green",
        only_flag: "fix-versions",
        leader_keys: "<leader>ijv",
        command_id: "jira_fix_versions.open",
        command_title: "Jira Fix Versions: open",
    },
    SplitChip {
        id: "jira_boards",
        name: "Jira Boards",
        description: "Jira: active sprint + backlog",
        fallback: "JB",
        label: "Jira Boards",
        color: "magenta",
        only_flag: "boards",
        leader_keys: "<leader>ijb",
        command_id: "jira_boards.open",
        command_title: "Jira Boards: open",
    },
];

pub fn install() -> Result<()> {
    // Remove the legacy combined manifest first so a fresh install
    // doesn't leave four chips in the rail. Silent if it's already
    // gone — common case for new users.
    if uninstall_integration(LEGACY_ID)? {
        println!("removed legacy combined manifest ({LEGACY_ID}) — replaced by three split chips");
    }

    for chip in SPLITS {
        let spec = build_spec(chip);
        let path = install_integration(&spec)?;
        println!("wrote manifest: {}", path.display());
    }
    println!("\nrun mnml + `integrations.refresh` (or restart) to pick up the new chips");
    println!("chords: <leader>ijw / ijv / ijb  ·  right-click chip for options");
    Ok(())
}

/// Auth-field schema shared across all three chips (Work / Fix Versions /
/// Boards). All three run the same binary + hit the same Jira Cloud API,
/// so they share one auth surface: site URL + email + API token. Also
/// carries `bitbucket_access_token` because jira_fix_versions correlates
/// tickets with linked BB PRs (see `src/bitbucket.rs`).
///
/// mnml reads these declarations to (a) render the per-integration
/// Settings pane form, (b) intercept a chip click with missing required
/// auth and open the pane, (c) inject env vars at Pty spawn from
/// `[auth_values]` via `env_fallback` so users who type a token into
/// the pane don't have to also edit their shell rc file.
///
/// Added in 0.2.6 (2026-08-11); requires `mnml-bridge = "0.7"`.
fn auth_fields() -> Vec<AuthField> {
    vec![
        AuthField {
            key: "site_url".into(),
            label: "Jira site URL".into(),
            kind: "url".into(),
            env_fallback: Some("JIRA_URL".into()),
            help: Some(
                "Your Atlassian instance, like https://mycompany.atlassian.net".into(),
            ),
            required: true,
            ..Default::default()
        },
        AuthField {
            key: "email".into(),
            label: "Atlassian account email".into(),
            kind: "email".into(),
            env_fallback: Some("JIRA_EMAIL".into()),
            help: Some(
                "The email you use to sign in to Jira. Pairs with the API token for HTTP Basic auth."
                    .into(),
            ),
            required: true,
            ..Default::default()
        },
        AuthField {
            key: "api_token".into(),
            label: "Jira API token".into(),
            kind: "secret".into(),
            env_fallback: Some("JIRA_API_TOKEN".into()),
            help_url: Some("https://id.atlassian.com/manage-profile/security/api-tokens".into()),
            help: Some(
                "Create at id.atlassian.com → Security → API tokens. This is different from your Atlassian account password."
                    .into(),
            ),
            required: true,
            ..Default::default()
        },
        AuthField {
            key: "bitbucket_access_token".into(),
            label: "Bitbucket access token (optional)".into(),
            kind: "secret".into(),
            env_fallback: Some("BITBUCKET_ACCESS_TOKEN".into()),
            help: Some(
                "Optional. Fix Versions view uses this to fetch the PRs linked from each Jira ticket. Skip if you don't need PR correlation."
                    .into(),
            ),
            required: false,
            ..Default::default()
        },
    ]
}

fn build_spec(chip: &SplitChip) -> IntegrationSpec {
    IntegrationSpec {
            id: chip.id.into(),
            label: chip.label.into(),
            description: Some(chip.description.into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            binary: "mnml-tracker-jira".into(),
            category: Some("tracker".into()),
            chip: Some(ChipSpec {
                // nf-md-jira glyph everywhere — the three chips
                // are one family; the label / fallback letters
                // split them apart.
                glyph: "\u{F0303}".into(),
                fallback: chip.fallback.into(),
                color: chip.color.into(),
                enabled: true,
                in_palette_bar: false,
                badge_key: Some(chip.id.into()),
                ..Default::default()
            }),
            commands: vec![CommandSpec {
                id: chip.command_id.into(),
                title: chip.command_title.into(),
                group: Some("integrations".into()),
                keys: vec![chip.leader_keys.into()],
                run: format!(":term mnml-tracker-jira --only {}", chip.only_flag),
            }],
            auth: auth_fields(),
            ..Default::default()
    }
}

pub fn uninstall() -> Result<()> {
    let mut any = false;
    // Legacy first (in case the user is only on the pre-split
    // shape), then the three splits.
    for id in [LEGACY_ID].into_iter().chain(SPLITS.iter().map(|c| c.id)) {
        if uninstall_integration(id)? {
            println!("removed manifest for {id}");
            any = true;
        }
    }
    if !any {
        println!("no jira manifests present (already uninstalled)");
    }
    Ok(())
}
