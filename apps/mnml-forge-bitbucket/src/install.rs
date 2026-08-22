//! `--install` / `--uninstall` subcommands.
//!
//! 2026-08-07 — split into rail chips matching the `mnml-tracker-jira`
//! pattern. Each chip drops the user straight into a single-purpose
//! view via `--only <family>`:
//!   - Bitbucket PRs        → `mnml-forge-bitbucket --only prs`
//!   - Bitbucket Pipelines  → `mnml-forge-bitbucket --only pipelines`
//!
//! 2026-08-08 — Branches chip removed. User report: the branches
//! surface was accidentally shipped by an earlier iteration and
//! doesn't reflect a real supported view. `--uninstall` still tries
//! to remove `bitbucket_branches` so anyone who installed the
//! branches chip while it was there gets it cleaned up.
//!
//! The pre-split combined "bitbucket" manifest gets removed on
//! install so it doesn't duplicate as a third chip. `--uninstall`
//! removes all four (the two current splits + legacy combined +
//! retired branches) for symmetry with a user who's been on any of
//! the historical schemes.

use anyhow::Result;
use mnml_bridge::{
    AuthField, ChipSpec, CommandSpec, IntegrationSpec, install_integration, uninstall_integration,
};

const LEGACY_ID: &str = "bitbucket";

/// One rail chip's registration data. `id` doubles as the manifest
/// filename (`~/.config/mnml/integrations/<id>.toml`) and the
/// mnml-side chip id for right-click / config overrides.
struct SplitChip {
    id: &'static str,
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

/// The two split chips. Same glyph (Bitbucket icon, E703) across
/// both — the tooltip + fallback letters distinguish them.
/// Colors: prs=blue (review flow), pipelines=green (build health).
const SPLITS: &[SplitChip] = &[
    SplitChip {
        id: "bitbucket_prs",
        description: "Bitbucket: open + merged pull requests across the workspace",
        fallback: "BP",
        label: "Bitbucket PRs",
        color: "blue",
        only_flag: "prs",
        leader_keys: "<leader>ibp",
        command_id: "bitbucket_prs.open",
        command_title: "Bitbucket PRs: open",
    },
    SplitChip {
        id: "bitbucket_pipelines",
        description: "Bitbucket: recent pipeline runs per repo/branch",
        fallback: "BL",
        label: "Bitbucket Pipelines",
        color: "green",
        only_flag: "pipelines",
        leader_keys: "<leader>ibl",
        command_id: "bitbucket_pipelines.open",
        command_title: "Bitbucket Pipelines: open",
    },
];

/// Retired chips — install won't create them, but `--uninstall` will
/// remove them if a user is still on an older install schema.
const RETIRED_IDS: &[&str] = &["bitbucket_branches"];

/// Auth fields written into each chip's manifest. mnml reads these
/// and (a) surfaces the form via right-click → "Configure…",
/// (b) intercepts a chip click with required-missing auth to open
/// that form, (c) injects the env vars from `[auth_values]` at
/// Pty spawn time using the `env_fallback` names — so a user who
/// pastes a token into the pane sees it flow through to the
/// sibling as `$BITBUCKET_APP_PASSWORD` without editing their
/// shell rc file. Env-var users are unaffected (skip-if-empty
/// leaves their shell export in place).
///
/// Two chips (PRs + Pipelines) share the same binary + auth path,
/// so both manifests carry the same block. Added in 0.1.4
/// (2026-08-11); requires `mnml-bridge = "0.7"`.
fn auth_fields() -> Vec<AuthField> {
    vec![
        // Preferred (2026-08+): Atlassian scoped api_token. Drawn
        // from a separate rate-limit bucket than app passwords, and
        // revocable per-integration without invalidating the rest of
        // your automation. Not marked `required` so users on the
        // legacy app_password path still validate — the loader takes
        // whichever is set (api_token wins if both are).
        AuthField {
            key: "api_token".into(),
            label: "Bitbucket API token (recommended)".into(),
            kind: "secret".into(),
            env_fallback: Some("BITBUCKET_API_TOKEN".into()),
            help_url: Some(
                "https://id.atlassian.com/manage-profile/security/api-tokens"
                    .into(),
            ),
            help: Some(
                "Preferred. Atlassian scoped API token — fresh rate-limit bucket, per-integration revoke. Either this OR an app password is required."
                    .into(),
            ),
            required: false,
        },
        AuthField {
            key: "app_password".into(),
            label: "Bitbucket app password".into(),
            kind: "secret".into(),
            env_fallback: Some("BITBUCKET_APP_PASSWORD".into()),
            help_url: Some(
                "https://bitbucket.org/account/settings/app-passwords/"
                    .into(),
            ),
            help: Some(
                "Legacy path. Set this OR an api_token above (api_token wins if both are set). Scopes: Repositories:Read + Pull requests:Read + Pipelines:Read."
                    .into(),
            ),
            required: false,
        },
        AuthField {
            key: "username".into(),
            label: "Bitbucket username (optional)".into(),
            kind: "text".into(),
            env_fallback: Some("BITBUCKET_USERNAME".into()),
            help: Some(
                "Optional. Only needed if your app password requires Basic auth (username + password); leave blank for token-only auth."
                    .into(),
            ),
            required: false,
            ..Default::default()
        },
    ]
}

pub fn install() -> Result<()> {
    // Remove the legacy combined manifest first so a fresh install
    // doesn't leave four chips in the rail. Silent if it's already
    // gone — common case for new users.
    if uninstall_integration(LEGACY_ID)? {
        println!("removed legacy combined manifest ({LEGACY_ID}) — replaced by three split chips");
    }

    for chip in SPLITS {
        let spec = IntegrationSpec {
            id: chip.id.into(),
            label: chip.label.into(),
            description: Some(chip.description.into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            binary: "mnml-forge-bitbucket".into(),
            category: Some("forge".into()),
            chip: Some(ChipSpec {
                // nf-dev-bitbucket glyph everywhere — the three chips
                // are one family; the label / fallback letters split
                // them apart.
                glyph: "\u{F00A8}".into(),
                fallback: chip.fallback.into(),
                color: chip.color.into(),
                enabled: true,
                in_palette_bar: false,
                badge_key: Some(chip.id.into()),
                ..Default::default()
            }),
            commands: {
                let mut cmds = vec![CommandSpec {
                    id: chip.command_id.into(),
                    title: chip.command_title.into(),
                    group: Some("integrations".into()),
                    keys: vec![chip.leader_keys.into()],
                    run: format!(":term mnml-forge-bitbucket --only {}", chip.only_flag),
                }];
                // #1099 (2026-08-20) — the PRs chip's statusline
                // segment counts "open PRs I authored", so a click
                // should land on the mine-only view, not the full PR
                // family. Register a second command that filters via
                // `--only prs-mine`; the segment's `click_command`
                // points here while the leader chord + palette entry
                // keep the wider `--only prs` view.
                if chip.id == "bitbucket_prs" {
                    cmds.push(CommandSpec {
                        id: "bitbucket_prs.open_mine".into(),
                        title: "Bitbucket PRs: open (mine only)".into(),
                        group: Some("integrations".into()),
                        keys: vec![],
                        run: ":term mnml-forge-bitbucket --only prs-mine".into(),
                    });
                }
                cmds
            },
            auth: auth_fields(),
            ..Default::default()
        };
        let path = install_integration(&spec)?;
        // mnml 0.2.11+ generic statusline-segment surface. The
        // schema (`[[values_sources]]` + `[[statusline_segments]]`)
        // lives in mnml-bridge 0.8+, which isn't on crates.io yet —
        // this sibling still resolves 0.7. Rather than block on the
        // publish, we append the sections as raw TOML after the
        // bridge writes its portion. Only the PRs chip carries a
        // segment for now — the Pipelines chip has no bounded "how
        // many are red right now" summary that fits a right-side
        // chip yet. See docs/design/statusline-segments.md on the
        // mnml side.
        if let Err(e) = append_segment_blocks(chip.id, &path) {
            eprintln!(
                "note: couldn't append [[values_sources]] to {} ({e}) — hand-edit that file to add the chip",
                path.display()
            );
        }
        // #1117 (2026-08-21) — background prefetch. Bridge 0.7
        // doesn't know about `[[prefetch]]` either, so we raw-append
        // the block like we do for statusline segments. mnml core
        // polls this command in the background and stamps the
        // resulting cache path on the child env; the interactive
        // launch hydrates from it. Idempotent — the id-uniqueness
        // grep in append_prefetch_block prevents doubling on
        // re-install.
        if let Err(e) = append_prefetch_block(chip.id, chip.only_flag, &path) {
            eprintln!(
                "note: couldn't append [[prefetch]] to {} ({e}) — hand-edit that file to add the block",
                path.display()
            );
        }
        println!("wrote manifest: {}", path.display());
    }
    println!("\nrun mnml + `integrations.refresh` (or restart) to pick up the new chips");
    println!("chords: <leader>ibp / ibl / ibb  ·  right-click chip for options");
    Ok(())
}

/// Append mnml 0.2.11+ statusline-segment TOML blocks to a
/// freshly-written manifest, but only for the PRs chip. Idempotent:
/// re-installing does NOT double-append (we grep the file for the
/// segment `id` first). No-ops for chips that don't declare a
/// segment. See main.rs `--values` for the paired data source.
fn append_segment_blocks(chip_id: &str, path: &std::path::Path) -> std::io::Result<()> {
    // Only the PRs chip has a segment declaration today.
    if chip_id != "bitbucket_prs" {
        return Ok(());
    }
    let current = std::fs::read_to_string(path)?;
    // Idempotence guard — the segment `id` is unique enough to
    // detect a prior append cleanly. Prevents a second install
    // (or a re-install to pick up a new sibling version) from
    // stacking duplicate `[[values_sources]]` entries.
    const SEGMENT_ID: &str = "bitbucket_prs_mine";
    if current.contains(SEGMENT_ID) {
        return Ok(());
    }
    // Raw TOML append. Bridge 0.7 doesn't know about these
    // sections; bridge 0.8+ will serialize them from typed structs,
    // and this whole helper can be deleted then. Glyph is
    // nf-dev-bitbucket (U+F00A8) — matches the parent chip so the
    // family reads consistently in the statusline. Was U+F062D
    // (source-pull) but user asked to unify on the Bitbucket logo.
    let block = concat!(
        "\n",
        "# mnml 0.2.11+ statusline segment — appended by\n",
        "# mnml-forge-bitbucket --install. Idempotent: re-install\n",
        "# skips this if `bitbucket_prs_mine` is already present.\n",
        "[[values_sources]]\n",
        "id = \"bitbucket_values\"\n",
        "command = \"mnml-forge-bitbucket --values\"\n",
        "poll_interval_secs = 300\n",
        "\n",
        "[[statusline_segments]]\n",
        "id = \"bitbucket_prs_mine\"\n",
        "source = \"bitbucket_values\"\n",
        "glyph = \"\u{F00A8}\"\n",
        // 2026-08-20 — Bitbucket-brand green (#8BBF4E) for the
        // chip bg. mnml accepts `#RRGGBB` in a statusline segment
        // `color` alongside the theme keys.
        "color = \"#8BBF4E\"\n",
        "format = \"{open_mine}({unapproved_mine})\"\n",
        // #1126 (2026-08-22) — spell out the `!` failure state so a
        // user staring at a red bang has somewhere to start. mnml
        // core paints `!` when the `--values` fetch returns a
        // non-zero exit or fails to parse; the two realistic causes
        // are an expired app_password (Bitbucket returns 401) and a
        // per-user rate ceiling (429). Both surface a stderr line
        // from the poller too — see `mnml --logs`.
        "tooltip = \"Open PRs you authored (last 90 days, non-release) — parens = still-needs-review count. `!` = fetch failed (usually expired token or rate-limited; see mnml --logs). Click to open the mine-only PRs tab.\"\n",
        "click_command = \"bitbucket_prs.open_mine\"\n",
    );
    // Ensure a trailing newline before we append so we never fuse
    // a section onto the last line of an existing block.
    let mut out = current;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    std::fs::write(path, out)
}

/// #1117 (2026-08-21) — append a `[[prefetch]]` block to a
/// freshly-written chip manifest. Bridge 0.7 doesn't serialize this
/// section yet (mnml core's `PrefetchSource` schema lives in
/// mnml/src/integration_manifest.rs); once bridge 0.8+ ships typed
/// support the whole helper can be deleted.
///
/// Idempotent — a re-install with the same prefetch id is a no-op.
/// `for_pane_kind` matches the mnml-side pane kind mnml core opens
/// when the split chip fires: workspace_open_prs for the PRs chip,
/// workspace_pipelines for the Pipelines chip. Poll cadence at
/// 600s (10 min) mirrors the jira `[[values_sources]]` default and
/// is comfortably under Bitbucket's per-workspace rate ceiling.
fn append_prefetch_block(
    chip_id: &str,
    only_flag: &str,
    path: &std::path::Path,
) -> std::io::Result<()> {
    // The two split chips each get one prefetch entry. Nothing
    // else — the legacy combined manifest is removed at install
    // start, and retired chips are cleaned in `uninstall`.
    let (prefetch_id, for_pane_kind) = match chip_id {
        "bitbucket_prs" => ("bitbucket_prs_prefetch", "workspace_open_prs"),
        "bitbucket_pipelines" => ("bitbucket_pipelines_prefetch", "workspace_pipelines"),
        _ => return Ok(()),
    };
    let current = std::fs::read_to_string(path)?;
    // Idempotence guard — prefetch id is unique across the two chips.
    if current.contains(prefetch_id) {
        return Ok(());
    }
    let block = format!(
        "\n# mnml 0.2.11+ background prefetch — appended by\n\
         # mnml-forge-bitbucket --install. Idempotent: re-install\n\
         # skips this if `{prefetch_id}` is already present.\n\
         [[prefetch]]\n\
         id = \"{prefetch_id}\"\n\
         command = \"mnml-forge-bitbucket --prefetch --only {only_flag}\"\n\
         poll_interval_secs = 600\n\
         for_pane_kind = \"{for_pane_kind}\"\n",
    );
    let mut out = current;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&block);
    std::fs::write(path, out)
}

pub fn uninstall() -> Result<()> {
    let mut any = false;
    // Legacy combined first, then the current splits, then any
    // retired split chips (bitbucket_branches, etc.) so a user on
    // ANY historical install shape gets a clean sweep.
    for id in [LEGACY_ID]
        .into_iter()
        .chain(SPLITS.iter().map(|c| c.id))
        .chain(RETIRED_IDS.iter().copied())
    {
        if uninstall_integration(id)? {
            println!("removed manifest for {id}");
            any = true;
        }
    }
    if !any {
        println!("no bitbucket manifests present (already uninstalled)");
    }
    Ok(())
}
