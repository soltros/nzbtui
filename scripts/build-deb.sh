#!/usr/bin/env bash
set -euo pipefail
umask 022

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PKG_NAME="nzbtui"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
ARCH="$(dpkg --print-architecture)"
OUT_DIR="target/debian"
STAGE_DIR="$OUT_DIR/stage_${PKG_NAME}_${VERSION}_${ARCH}"
DEB_PATH="$OUT_DIR/${PKG_NAME}_${VERSION}_${ARCH}.deb"

echo "==> Building release binary for ${PKG_NAME} v${VERSION} (${ARCH})..."
cargo build --release --locked --offline 2>/dev/null || cargo build --release --locked || cargo build --release

rm -rf "$STAGE_DIR"
mkdir -p \
  "$STAGE_DIR/DEBIAN" \
  "$STAGE_DIR/usr/bin" \
  "$STAGE_DIR/usr/share/doc/${PKG_NAME}" \
  "$STAGE_DIR/usr/share/man/man1"

install -m 0755 "target/release/${PKG_NAME}" "$STAGE_DIR/usr/bin/${PKG_NAME}"
install -m 0644 "README.md" "$STAGE_DIR/usr/share/doc/${PKG_NAME}/README.md"
install -m 0644 "LICENSE" "$STAGE_DIR/usr/share/doc/${PKG_NAME}/copyright"
gzip -n -9 -c "debian/${PKG_NAME}.1" > "$STAGE_DIR/usr/share/man/man1/${PKG_NAME}.1.gz"
chmod 0644 "$STAGE_DIR/usr/share/man/man1/${PKG_NAME}.1.gz"

INSTALLED_SIZE="$(du -sk "$STAGE_DIR/usr" | awk '{print $1}')"

cat > "$STAGE_DIR/DEBIAN/control" <<EOF
Package: ${PKG_NAME}
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${INSTALLED_SIZE}
Depends: libc6
Recommends: par2, 7zip | p7zip-full, unrar | unrar-free, unzip
Maintainer: Derrik Diener <142091676+soltros@users.noreply.github.com>
Description: Standalone Rust TUI Usenet client built with Ratatui
 Standalone terminal Usenet client without any web server or daemon.
 Supports direct Newznab indexer search, multiple NNTP/TLS providers with
 priority fallback, yEnc decoding with CRC verification, on-demand PAR2
 verification and repair, and automatic archive extraction.
EOF

chmod 0755 "$STAGE_DIR/DEBIAN"
chmod 0644 "$STAGE_DIR/DEBIAN/control"

mkdir -p "$OUT_DIR"
dpkg-deb --root-owner-group --build "$STAGE_DIR" "$DEB_PATH"

echo "==> Package created: ${DEB_PATH}"
dpkg-deb --info "$DEB_PATH"
dpkg-deb --contents "$DEB_PATH"
