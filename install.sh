#!/bin/sh
# rumor installer: download the latest prebuilt binary and put it on your PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/ornitech/rumor/main/install.sh | sh
#
# Env overrides:
#   RUMOR_VERSION       version tag to install (e.g. v0.11.0). Default: latest release.
#   RUMOR_INSTALL_DIR   where to install. Default: /usr/local/bin if writable, else ~/.local/bin.
set -eu

REPO="ornitech/rumor"

err() {
    echo "rumor-install: $*" >&2
    exit 1
}

# --- detect platform -------------------------------------------------------
os="$(uname -s)"
[ "$os" = "Darwin" ] || err "prebuilt binaries are macOS-only.
Build from source (https://github.com/${REPO}) or use Homebrew (brew install rumor) instead."

arch="$(uname -m)"
case "$arch" in
    arm64) target="aarch64-apple-darwin" ;;
    x86_64) target="x86_64-apple-darwin" ;;
    *) err "unsupported architecture: $arch" ;;
esac

# --- pick a downloader -----------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    dl() { curl -fsSL "$1"; }
    dl_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    dl() { wget -qO- "$1"; }
    dl_to() { wget -qO "$2" "$1"; }
else
    err "need curl or wget to download"
fi

# --- resolve version -------------------------------------------------------
tag="${RUMOR_VERSION:-}"
if [ -z "$tag" ]; then
    tag="$(dl "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | head -n1 \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
    [ -n "$tag" ] || err "could not determine the latest release"
fi

artifact="rumor-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${tag}"

# --- download into a temp dir ---------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "rumor-install: downloading $artifact ($tag)"
dl_to "${base}/${artifact}" "${tmp}/${artifact}" || err "download failed: ${base}/${artifact}"

# --- verify checksum (warn-and-continue if the sidecar is missing) --------
if dl_to "${base}/${artifact}.sha256" "${tmp}/${artifact}.sha256" 2>/dev/null; then
    if command -v shasum >/dev/null 2>&1; then
        ( cd "$tmp" && shasum -a 256 -c "${artifact}.sha256" >/dev/null ) || err "checksum verification failed"
    elif command -v sha256sum >/dev/null 2>&1; then
        ( cd "$tmp" && sha256sum -c "${artifact}.sha256" >/dev/null ) || err "checksum verification failed"
    else
        echo "rumor-install: warning: no shasum/sha256sum found, skipping verification" >&2
    fi
else
    echo "rumor-install: warning: no checksum published for ${tag}, skipping verification" >&2
fi

# --- extract ---------------------------------------------------------------
tar -xzf "${tmp}/${artifact}" -C "$tmp" || err "failed to extract $artifact"
[ -f "${tmp}/rumor" ] || err "archive did not contain a rumor binary"

# --- resolve install dir ---------------------------------------------------
if [ -n "${RUMOR_INSTALL_DIR:-}" ]; then
    dest_dir="$RUMOR_INSTALL_DIR"
elif [ -w /usr/local/bin ] 2>/dev/null; then
    dest_dir="/usr/local/bin"
else
    dest_dir="$HOME/.local/bin"
fi

mkdir -p "$dest_dir" || err "could not create $dest_dir"
install -m 755 "${tmp}/rumor" "${dest_dir}/rumor" || err "could not install to $dest_dir"

echo "rumor-install: installed ${dest_dir}/rumor ($tag)"

# --- PATH warning ----------------------------------------------------------
case ":$PATH:" in
    *":$dest_dir:"*) ;;
    *)
        echo "rumor-install: warning: $dest_dir is not on your PATH" >&2
        echo "  add it with: export PATH=\"$dest_dir:\$PATH\"" >&2
        ;;
esac

"${dest_dir}/rumor" --version 2>/dev/null || true
