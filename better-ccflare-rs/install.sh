#!/bin/sh
# better-ccflare Rust binary installer
# Usage: curl -fsSL https://raw.githubusercontent.com/SijanC147/better-ccflare/main/better-ccflare-rs/install.sh | sh
set -e

REPO="SijanC147/better-ccflare"
INSTALL_DIR="/usr/local/bin"
BINARY_NAME="better-ccflare"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64)
        ARTIFACT="${BINARY_NAME}-linux-amd64"
        ;;
    aarch64|arm64)
        ARTIFACT="${BINARY_NAME}-linux-arm64"
        ;;
    *)
        echo "Error: Unsupported architecture: $ARCH"
        echo "Supported: x86_64 (amd64), aarch64 (arm64)"
        exit 1
        ;;
esac

# Detect OS
OS=$(uname -s)
if [ "$OS" != "Linux" ]; then
    echo "Error: This installer only supports Linux."
    echo "Detected OS: $OS"
    exit 1
fi

# Find latest rust release tag
echo "Detecting latest release..."
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases" \
    | grep -o '"tag_name": *"rust-v[^"]*"' \
    | head -1 \
    | cut -d'"' -f4)

if [ -z "$TAG" ]; then
    echo "Error: Could not find a Rust release."
    exit 1
fi

VERSION="${TAG#rust-}"
echo "Latest version: ${VERSION}"
echo "Architecture:   ${ARCH}"
echo "Artifact:       ${ARTIFACT}"
echo ""

# Download binary
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${ARTIFACT}"
CHECKSUM_URL="${DOWNLOAD_URL}.sha256"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${ARTIFACT}..."
curl -fsSL -o "${TMPDIR}/${ARTIFACT}" "$DOWNLOAD_URL"

echo "Downloading checksum..."
curl -fsSL -o "${TMPDIR}/${ARTIFACT}.sha256" "$CHECKSUM_URL"

# Verify checksum
echo "Verifying checksum..."
cd "$TMPDIR"
if sha256sum -c "${ARTIFACT}.sha256"; then
    echo "Checksum verified."
else
    echo "Error: Checksum verification failed!"
    exit 1
fi

# Install
echo ""
echo "Installing to ${INSTALL_DIR}/${BINARY_NAME}..."
if [ -w "$INSTALL_DIR" ]; then
    cp "${TMPDIR}/${ARTIFACT}" "${INSTALL_DIR}/${BINARY_NAME}"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
else
    sudo cp "${TMPDIR}/${ARTIFACT}" "${INSTALL_DIR}/${BINARY_NAME}"
    sudo chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
fi

echo ""
echo "better-ccflare ${VERSION} installed successfully!"
echo ""
echo "Run 'better-ccflare --help' to get started."
