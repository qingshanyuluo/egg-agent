#!/bin/sh
set -e

REPO="qingshanyuluo/egg-agent"
BIN="egg"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

# ---- detect platform ----
OS=$(uname -s)
ARCH=$(uname -m)

case "$OS-$ARCH" in
  Linux-x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  Darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
  Darwin-arm64)  TARGET="aarch64-apple-darwin" ;;
  *)
    echo "Unsupported platform: $OS / $ARCH" >&2
    exit 1
    ;;
esac

# ---- fetch latest release ----
echo "Fetching latest release of egg-agent…"
RELEASE_URL="https://github.com/$REPO/releases/latest/download/egg-$TARGET.tar.gz"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

cd "$TMPDIR"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$RELEASE_URL" -o egg.tar.gz
elif command -v wget >/dev/null 2>&1; then
  wget -q "$RELEASE_URL" -O egg.tar.gz
else
  echo "neither curl nor wget found" >&2
  exit 1
fi

tar xzf egg.tar.gz

# ---- install ----
if [ -w "$INSTALL_DIR" ]; then
  cp "$BIN" "$INSTALL_DIR/$BIN"
else
  echo "Need sudo to install to $INSTALL_DIR"
  sudo cp "$BIN" "$INSTALL_DIR/$BIN"
fi
chmod +x "$INSTALL_DIR/$BIN"

echo "egg-agent installed to $INSTALL_DIR/$BIN"
echo "Run 'egg config' to set your API key."
