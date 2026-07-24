#!/bin/sh
set -e

REPO="qingshanyuluo/egg-agent"
BIN="egg"
# Empty unless the user explicitly overrides. The install step below picks a
# sensible default so `curl … | sh` works without sudo or a controlling TTY.
INSTALL_DIR="${INSTALL_DIR:-}"

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
  # --connect-timeout bounds the initial handshake so a black-holed network
  # fails fast instead of hanging; --retry with backoff rides out transient
  # blips (flaky wifi, 5xx from the CDN). --retry-all-errors so connection
  # failures (not just HTTP codes) are retried too. --fail keeps a 404 from
  # being written to disk as a "success".
  if ! curl -fsSL \
        --connect-timeout 10 \
        --retry 3 --retry-delay 2 --retry-all-errors \
        "$RELEASE_URL" -o egg.tar.gz; then
    echo "Download failed: $RELEASE_URL" >&2
    echo "Check your network, or see https://github.com/$REPO/releases for manual install." >&2
    exit 1
  fi
elif command -v wget >/dev/null 2>&1; then
  if ! wget --timeout=10 --tries=3 --waitretry=2 -q "$RELEASE_URL" -O egg.tar.gz; then
    echo "Download failed: $RELEASE_URL" >&2
    echo "Check your network, or see https://github.com/$REPO/releases for manual install." >&2
    exit 1
  fi
else
  echo "neither curl nor wget found" >&2
  exit 1
fi

# Guard against a truncated / empty download slipping through.
if [ ! -s egg.tar.gz ]; then
  echo "Downloaded archive is empty — aborting." >&2
  exit 1
fi

tar xzf egg.tar.gz

# ---- install ----
# Copy the binary into $1 (using sudo only if we have a real TTY to prompt on).
# Returns non-zero if the dir isn't writable and we can't escalate — the caller
# then falls back to a user-owned dir.
install_bin() {
  dir="$1"
  if [ -w "$dir" ]; then
    cp "$BIN" "$dir/$BIN" && chmod +x "$dir/$BIN"
  elif command -v sudo >/dev/null 2>&1 && [ -t 0 ]; then
    echo "Need sudo to install to $dir"
    sudo cp "$BIN" "$dir/$BIN" && sudo chmod +x "$dir/$BIN"
  else
    return 1
  fi
}

if [ -n "$INSTALL_DIR" ]; then
  # Explicit override — honor it, creating the dir if needed.
  mkdir -p "$INSTALL_DIR" 2>/dev/null || true
  if ! install_bin "$INSTALL_DIR"; then
    echo "Cannot write to $INSTALL_DIR (and no interactive sudo available)." >&2
    exit 1
  fi
  DEST="$INSTALL_DIR"
elif [ -w /usr/local/bin ] && install_bin /usr/local/bin; then
  # System dir already writable (e.g. Homebrew-owned, or running as root).
  DEST="/usr/local/bin"
else
  # Default: a user-owned dir that needs no sudo. Works under `curl | sh`.
  DEST="$HOME/.local/bin"
  mkdir -p "$DEST"
  if ! install_bin "$DEST"; then
    echo "Failed to install to $DEST" >&2
    exit 1
  fi
fi

echo "egg-agent installed to $DEST/$BIN"

# Nudge the user if the install dir isn't on PATH — otherwise `egg` won't be found.
case ":$PATH:" in
  *":$DEST:"*) ;;
  *)
    echo "⚠  $DEST is not on your PATH. Add it, then restart your shell:"
    echo "     echo 'export PATH=\"$DEST:\$PATH\"' >> ~/.zshrc"
    ;;
esac

echo "Run 'egg' to start, then /connect to add your API provider."
