#!/usr/bin/env bash
# Build (if needed) and run rumor from any directory, without cd-ing into the
# repo or typing cargo. All arguments are passed straight through to rumor.
#
# Usage:
#   scripts/run.sh [config.json] [rumor args...]
#   RUMOR_RELEASE=1 scripts/run.sh ...   # use the optimized release build
#
# Relative paths (e.g. a config arg) resolve against your current directory,
# not the repo, so this behaves like running the binary directly. Symlink it
# onto your PATH (e.g. `ln -s "$PWD/scripts/run.sh" ~/.local/bin/rumor-dev`)
# to launch from anywhere. For a standalone install, see install-local.sh.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

profile_args=()
if [[ "${RUMOR_RELEASE:-}" == "1" ]]; then
    profile_args=(--release)
fi

exec cargo run --quiet ${profile_args[@]+"${profile_args[@]}"} \
    --manifest-path "$repo_root/Cargo.toml" -- "$@"
