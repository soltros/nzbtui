#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "Error: 'rpmbuild' is not installed." >&2
  echo "Install it on Fedora/RHEL with: sudo dnf install rpm-build" >&2
  echo "Or on Debian/Ubuntu with: sudo apt install rpm" >&2
  exit 1
fi

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
TOPDIR="${ROOT_DIR}/target/rpmbuild"
TARBALL="nzbtui-${VERSION}.tar.gz"

mkdir -p "$TOPDIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
sed "s/^Version:.*/Version:        ${VERSION}/" packaging/rpm/nzbtui.spec > "$TOPDIR/SPECS/nzbtui.spec"

git archive --format=tar.gz --prefix="nzbtui-${VERSION}/" -o "$TOPDIR/SOURCES/${TARBALL}" HEAD

rpmbuild --define "_topdir ${TOPDIR}" -ba "$TOPDIR/SPECS/nzbtui.spec"

echo "==> RPM artifacts:"
find "$TOPDIR/RPMS" "$TOPDIR/SRPMS" -type f -name '*.rpm' -print
