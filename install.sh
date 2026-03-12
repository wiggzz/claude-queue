#!/bin/sh
# Install cq (Claude Queue) from GitHub releases
# Usage: curl -fsSL https://raw.githubusercontent.com/wiggzz/claude-queue/main/install.sh | sh
set -e

REPO="wiggzz/claude-queue"
INSTALL_DIR="${CQ_INSTALL_DIR:-/usr/local/bin}"

# Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  OS_TARGET="unknown-linux-gnu" ;;
  Darwin) OS_TARGET="apple-darwin" ;;
  *)      echo "Error: unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH_TARGET="x86_64" ;;
  aarch64|arm64) ARCH_TARGET="aarch64" ;;
  *)             echo "Error: unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${ARCH_TARGET}-${OS_TARGET}"

# Get latest release tag
TAG="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')"

if [ -z "$TAG" ]; then
  echo "Error: could not determine latest release" >&2
  exit 1
fi

ARCHIVE="cq-${TAG}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"

echo "Downloading cq ${TAG} for ${TARGET}..."
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$URL" -o "${TMPDIR}/${ARCHIVE}"
tar xzf "${TMPDIR}/${ARCHIVE}" -C "$TMPDIR"

echo "Installing to ${INSTALL_DIR}/cq..."
if [ -w "$INSTALL_DIR" ]; then
  mv "${TMPDIR}/cq" "${INSTALL_DIR}/cq"
else
  sudo mv "${TMPDIR}/cq" "${INSTALL_DIR}/cq"
fi

echo "cq ${TAG} installed successfully!"
echo "Run 'cq --help' to get started."
