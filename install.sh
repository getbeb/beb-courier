#!/bin/sh
# Install beb-courier: detect the platform, fetch the latest release binary,
# verify its checksum, place it on PATH.
#
#   curl -fsSL https://getbeb.dev/courier.sh | sh
#
# BEB_COURIER_INSTALL_DIR overrides the destination (default ~/.local/bin).
set -eu

REPO=getbeb/beb-courier
BIN_DIR=${BEB_COURIER_INSTALL_DIR:-$HOME/.local/bin}

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-musl ;;
    *) echo "unsupported OS: $os; build from source with: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac
case "$arch" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) echo "unsupported architecture: $arch; build from source with: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac
target=$arch-$os

url=https://github.com/$REPO/releases/latest/download
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# curl's own 404 says nothing about what to do next, and this is the
# first command a new user runs.
fetch() {
    curl -fsSL "$1" -o "$2" && return 0
    echo "cannot download $1" >&2
    echo "if there is no release for this platform yet, build from source:" >&2
    echo "  cargo install --git https://github.com/$REPO" >&2
    exit 1
}

fetch "$url/beb-courier-$target" "$tmp/beb-courier"
fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS"

if command -v sha256sum >/dev/null 2>&1; then
    have=$(sha256sum "$tmp/beb-courier" | awk '{print $1}')
else
    have=$(shasum -a 256 "$tmp/beb-courier" | awk '{print $1}')
fi
want=$(awk -v f="beb-courier-$target" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
if [ -z "$want" ] || [ "$have" != "$want" ]; then
    echo "checksum mismatch for beb-courier-$target; not installed" >&2
    exit 1
fi

mkdir -p "$BIN_DIR"
install -m 755 "$tmp/beb-courier" "$BIN_DIR/beb-courier"
echo "installed $BIN_DIR/beb-courier"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not on PATH; add it to your shell profile" ;;
esac
command -v ssh >/dev/null 2>&1 ||
    echo "note: beb-courier carries mail over ssh; install openssh" >&2
command -v beb >/dev/null 2>&1 ||
    echo "note: beb-courier hands mail to beb, which is not on PATH: https://getbeb.dev" >&2
