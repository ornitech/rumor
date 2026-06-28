#!/usr/bin/env bash
# Build rumor in release mode and install it as `rumor-local` on the PATH.
# Usage: scripts/install-local.sh [dest-dir]   (default: ~/.local/bin)
# Re-run after making changes to update the installed binary.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest_dir="${1:-$HOME/.local/bin}"
dest="$dest_dir/rumor-local"

cargo build --release --manifest-path "$repo_root/Cargo.toml"

mkdir -p "$dest_dir"
install -m 755 "$repo_root/target/release/rumor" "$dest"

echo "installed $dest"
case ":$PATH:" in
    *":$dest_dir:"*) ;;
    *) echo "warning: $dest_dir is not on your PATH" >&2 ;;
esac
