#!/usr/bin/env bash
#
# dist-check — run cargo-dist's validation locally before pushing a tag.
#
# `dist plan` reads dist-workspace.toml + Cargo metadata, computes the
# matrix, checks WiX GUIDs + profile.dist + sibling path-deps. If it
# fails locally, the tag-and-push will fail in CI ~10 min later at
# the same point — but on the cloud's dime.
#
# `dist build --artifacts=global` builds the install scripts. It
# calls `cargo metadata`, so a missing sibling clone (e.g. the
# tmnl-protocol path-dep) shows up here. The CI's
# `build-global-artifacts` job hits the same call.
#
# `dist build --artifacts=local --target=<host>` builds the binary
# for the local host triple. Catches dep / compile issues without
# requiring `cargo-zigbuild` (which the full `--artifacts=local`
# would invoke for cross-targets). The other targets still need CI.
#
# Doesn't catch cross-platform-only failures (Windows-specific
# code, Linux-only system deps) — those still need CI. But ~80% of
# the config errors that cost CI minutes today are caught here in
# under a minute.
#
# Usage:
#   ./scripts/dist-check.sh          # plan + global + local-host
#   ./scripts/dist-check.sh plan     # just `dist plan` (fastest)

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v dist >/dev/null 2>&1; then
    echo "error: cargo-dist's \`dist\` binary not on PATH." >&2
    echo "       install with: cargo install cargo-dist --version 0.32.0" >&2
    exit 1
fi

MODE="${1:-full}"

echo "── dist plan ──"
dist plan

if [ "$MODE" = "plan" ]; then
    echo
    echo "✓ plan only — skipping build phases (pass no arg for full check)"
    exit 0
fi

echo
echo "── dist build --artifacts=global ──"
dist build --artifacts=global

echo
HOST_TRIPLE=$(rustc -vV | awk '/^host:/ {print $2}')
echo "── dist build --artifacts=local --target=${HOST_TRIPLE} ──"
dist build --artifacts=local --target="${HOST_TRIPLE}"

echo
echo "✓ local validation passed. ok to tag + push."
