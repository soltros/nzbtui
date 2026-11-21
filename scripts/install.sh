#!/usr/bin/env bash
set -euo pipefail

REPO="soltros/nzbtui"
ASSET_NAME="nzbtui-linux-x86_64"
VERSION="${NZBTUI_VERSION:-latest}"
SYSTEM_INSTALL=0

for arg in "$@"; do
  case "$arg" in
    --system)
      SYSTEM_INSTALL=1
      ;;
    --version=*)
      VERSION="${arg#--version=}"
      ;;
    -h|--help)
      cat <<EOF
Usage: install.sh [--system] [--version=vX.Y.Z]

Installs the standalone nzbtui Linux x86_64 binary from GitHub Releases.
  --system          Install to /usr/local/bin (default: ~/.local/bin)
  --version=vX.Y.Z  Install a specific release tag (default: latest)
EOF
      exit 0
      ;;
  esac
done

ARCH="$(uname -m)"
if [[ "$ARCH" != "x86_64" && "$ARCH" != "amd64" ]]; then
  echo "Error: prebuilt generic binary is currently for x86_64 (detected: $ARCH)." >&2
  echo "You can build from source with: cargo install --git https://github.com/${REPO}.git" >&2
  exit 1
fi

if [[ "$SYSTEM_INSTALL" -eq 1 || "$(id -u)" -eq 0 ]]; then
  INSTALL_DIR="/usr/local/bin"
else
  INSTALL_DIR="${HOME}/.local/bin"
fi

if [[ "$VERSION" == "latest" ]]; then
  BASE_URL="https://github.com/${REPO}/releases/latest/download"
else
  BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "==> Downloading ${ASSET_NAME} from ${BASE_URL}..."
curl -fsSL "${BASE_URL}/${ASSET_NAME}" -o "${TMP_DIR}/${ASSET_NAME}"

if curl -fsSL "${BASE_URL}/SHA256SUMS" -o "${TMP_DIR}/SHA256SUMS" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then
    echo "==> Verifying SHA256 checksum..."
    (cd "$TMP_DIR" && grep " ${ASSET_NAME}$" SHA256SUMS | sha256sum -c -)
  fi
fi

mkdir -p "$INSTALL_DIR"
install -m 0755 "${TMP_DIR}/${ASSET_NAME}" "${INSTALL_DIR}/nzbtui"

echo "==> Installed nzbtui to ${INSTALL_DIR}/nzbtui"
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
  echo "Note: ${INSTALL_DIR} is not in your PATH. Add it with:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi
echo "Run 'nzbtui' to start, or 'nzbtui --update' later to self-update."
