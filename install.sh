#!/bin/sh
# NioAI installer for Linux, macOS, and Termux
set -eu

REPO="nio-labs/nio"
DEFAULT_BIN_DIR="$HOME/.local/bin"

cleanup() {
    if [ -n "${STAGED:-}" ]; then rm -f "$STAGED"; fi
    if [ -n "${TMP_DIR:-}" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT INT TERM

echo "📦 NioAI installer"

# 1. Detect OS
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)
        if [ -n "${TERMUX_VERSION:-}" ] || [ -d "/data/data/com.termux" ]; then
            PLATFORM="linux-android"
            DEFAULT_BIN_DIR="${PREFIX:-/data/data/com.termux/files/usr}/bin"
        elif ldd --version 2>&1 | grep -iq musl; then
            PLATFORM="unknown-linux-musl"
        else
            PLATFORM="unknown-linux-gnu"
        fi
        ;;
    darwin)
        PLATFORM="apple-darwin"
        ;;
    *)
        echo "❌ Unsupported operating system: $OS" >&2
        echo "   For Windows, run install.ps1 in PowerShell." >&2
        exit 1
        ;;
esac

# 2. Detect architecture
case "$ARCH" in
    x86_64|amd64)
        TARGET_ARCH="x86_64"
        ;;
    aarch64|arm64)
        TARGET_ARCH="aarch64"
        ;;
    *)
        echo "❌ Unsupported architecture: $ARCH" >&2
        exit 1
        ;;
esac

TARGET="${TARGET_ARCH}-${PLATFORM}"
echo "🔍 Detected target: $TARGET"

# 3. Determine install destination
INSTALL_DIR="${NIO_INSTALL_DIR:-$DEFAULT_BIN_DIR}"
mkdir -p "$INSTALL_DIR"

TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'nio-install')"
ARCHIVE="nio-${TARGET}.tar.gz"

VERSION="${NIO_VERSION:-latest}"
if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ARCHIVE}"
    CHECKSUM_URL="https://github.com/${REPO}/releases/latest/download/SHA256SUMS"
else
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"
    CHECKSUM_URL="https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS"
fi

echo "⬇️  Downloading NioAI (${VERSION})..."
if command -v curl >/dev/null 2>&1; then
    curl --connect-timeout 10 --max-time 120 -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$ARCHIVE" || {
        echo "❌ Failed to download release archive from $DOWNLOAD_URL" >&2
        exit 1
    }
    curl --connect-timeout 10 --max-time 120 -fsSL "$CHECKSUM_URL" -o "$TMP_DIR/SHA256SUMS"
elif command -v wget >/dev/null 2>&1; then
    wget --timeout=30 --tries=1 -q "$DOWNLOAD_URL" -O "$TMP_DIR/$ARCHIVE" || {
        echo "❌ Failed to download release archive from $DOWNLOAD_URL" >&2
        exit 1
    }
    wget --timeout=30 --tries=1 -q "$CHECKSUM_URL" -O "$TMP_DIR/SHA256SUMS"
else
    echo "❌ Neither curl nor wget is available." >&2
    exit 1
fi

# 4. Require one exact checksum entry and a verification tool.
echo "🔒 Verifying checksum..."
EXPECTED="$(awk -v archive="$ARCHIVE" '$2 == archive || $2 == "*" archive {print $1}' "$TMP_DIR/SHA256SUMS")"
if [ "${#EXPECTED}" -ne 64 ] || printf '%s' "$EXPECTED" | LC_ALL=C grep -q '[^a-fA-F0-9]'; then
    echo "❌ Missing, invalid, or duplicate checksum for $ARCHIVE." >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$TMP_DIR/$ARCHIVE" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$TMP_DIR/$ARCHIVE" | awk '{print $1}')"
else
    echo "❌ Install sha256sum or shasum to verify the download." >&2
    exit 1
fi
if [ "$ACTUAL" != "$(printf '%s' "$EXPECTED" | tr '[:upper:]' '[:lower:]')" ]; then
    echo "❌ Checksum verification failed!" >&2
    exit 1
fi

# 5. Extract and install
echo "📂 Extracting archive..."
tar -xzf "$TMP_DIR/$ARCHIVE" -C "$TMP_DIR"

if [ ! -f "$TMP_DIR/nio" ]; then
    echo "❌ Release archive did not contain 'nio' binary." >&2
    exit 1
fi

chmod +x "$TMP_DIR/nio"
if [ -e "$INSTALL_DIR/nio" ]; then
    EXISTING="$("$INSTALL_DIR/nio" --version 2>/dev/null || true)"
    case "$EXISTING" in
        "nio "*" (NioAI)") ;;
        *) echo "❌ Refusing to replace an unrelated nio executable. Choose NIO_INSTALL_DIR." >&2; exit 1 ;;
    esac
fi
DOWNLOADED="$("$TMP_DIR/nio" --version)"
case "$DOWNLOADED" in
    "nio "*" (NioAI)") ;;
    *) echo "❌ Downloaded executable is not NioAI." >&2; exit 1 ;;
esac
if [ "$VERSION" != "latest" ] && [ "$DOWNLOADED" != "nio ${VERSION#v} (NioAI)" ]; then
    echo "❌ Downloaded executable has the wrong version." >&2
    exit 1
fi
STAGED="$(mktemp "$INSTALL_DIR/.nio-install.XXXXXX")"
cp "$TMP_DIR/nio" "$STAGED"
chmod 755 "$STAGED"
mv -f "$STAGED" "$INSTALL_DIR/nio"
STAGED=""

if [ "$PLATFORM" = "apple-darwin" ]; then
    xattr -cr "$INSTALL_DIR/nio" 2>/dev/null || true
    codesign --force --deep -s - "$INSTALL_DIR/nio" 2>/dev/null || true
fi

echo "✅ Installed nio to $INSTALL_DIR/nio"

# 6. Verify installation & PATH check
if ! echo ":$PATH:" | grep -q ":$INSTALL_DIR:"; then
    echo ""
    echo "⚠️  $INSTALL_DIR is not currently in your PATH."
    echo "   Add it to your shell configuration (e.g. ~/.bashrc or ~/.zshrc):"
    echo "   export PATH=\"$INSTALL_DIR:\$PATH\""
    echo ""
fi

if [ -x "$INSTALL_DIR/nio" ]; then
    "$INSTALL_DIR/nio" --version || true
    echo "🚀 Run 'nio' to start coding!"
fi
