#!/usr/bin/env bash
set -euo pipefail
umask 022

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PKG_NAME="nzbtui"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
PKGREL="0"
OUT_DIR="${ROOT_DIR}/target/alpine"
TARGET_SPEC="${1:-x86}"

case "$TARGET_SPEC" in
  x86|ish)
    ARCHES=("x86")
    ;;
  x86_64|amd64)
    ARCHES=("x86_64")
    ;;
  all)
    ARCHES=("x86" "x86_64")
    ;;
  *)
    echo "Usage: $0 [x86|x86_64|all]" >&2
    echo "  x86    - 32-bit Alpine musl APK (for iSH on iOS and 32-bit Alpine)" >&2
    echo "  x86_64 - 64-bit Alpine musl APK" >&2
    echo "  all    - Build both x86 and x86_64 APKs" >&2
    exit 1
    ;;
esac

mkdir -p "$OUT_DIR"

build_in_docker() {
  local arch="$1"
  local image=""
  local platform=""
  case "$arch" in
    x86)
      image="i386/alpine:latest"
      platform="linux/386"
      ;;
    x86_64)
      image="alpine:latest"
      platform="linux/amd64"
      ;;
  esac

  echo "==> Building Alpine APK for ${PKG_NAME} v${VERSION}-r${PKGREL} (${arch}) via ${image}..."

  # Create a clean source tarball of the current working tree (excluding target/ and .git/)
  local tmp_src
  tmp_src="$(mktemp -d)"
  trap 'rm -rf "$tmp_src"' RETURN
  tar \
    --exclude='./target' \
    --exclude='./.git' \
    --transform="s,^\.,${PKG_NAME}-${VERSION}," \
    -czf "${tmp_src}/${PKG_NAME}-${VERSION}.tar.gz" .

  docker run --rm \
    --platform "$platform" \
    -v "${tmp_src}:/src:ro" \
    -v "${OUT_DIR}:/out" \
    -v "nzbtui-alpine-cargo-cache-${arch}:/home/builder/.cargo" \
    "$image" sh -euxc "
      apk add --no-cache alpine-sdk cargo rust tar gzip file
      adduser -D builder
      addgroup builder abuild
      mkdir -p /home/builder/pkg /home/builder/packages /home/builder/.cargo
      chown -R builder:builder /home/builder

      cp /src/${PKG_NAME}-${VERSION}.tar.gz /home/builder/pkg/
      cat > /home/builder/pkg/APKBUILD <<'APKBUILD_EOF'
# Maintainer: Derrik Diener <142091676+soltros@users.noreply.github.com>
pkgname=${PKG_NAME}
pkgver=${VERSION}
pkgrel=${PKGREL}
pkgdesc=\"Standalone Rust TUI Usenet client built with Ratatui\"
url=\"https://github.com/soltros/nzbtui\"
arch=\"${arch}\"
license=\"GPL-3.0-or-later\"
makedepends=\"cargo\"
subpackages=\"${PKG_NAME}-doc\"
options=\"!check net\"
source=\"${PKG_NAME}-${VERSION}.tar.gz\"

build() {
	local target
	target=\"\$(rustc -vV | sed -n 's/^host: //p')\"
	export CARGO_HOME=/home/builder/.cargo
	RUSTFLAGS=\"-C target-feature=+crt-static\" cargo build --release --locked --target \"\$target\"
}

package() {
	local target
	target=\"\$(rustc -vV | sed -n 's/^host: //p')\"
	install -Dm755 \"target/\$target/release/${PKG_NAME}\" \"\$pkgdir\"/usr/bin/${PKG_NAME}
	install -Dm644 debian/${PKG_NAME}.1 \"\$pkgdir\"/usr/share/man/man1/${PKG_NAME}.1
	install -Dm644 README.md \"\$pkgdir\"/usr/share/doc/${PKG_NAME}/README.md
	install -Dm644 LICENSE \"\$pkgdir\"/usr/share/licenses/${PKG_NAME}/LICENSE
}
APKBUILD_EOF

      chown -R builder:builder /home/builder/pkg
      su - builder -c 'abuild-keygen -a -n'
      cp /home/builder/.abuild/*.rsa.pub /etc/apk/keys/
      su - builder -c '
        set -eux
        cd /home/builder/pkg
        abuild checksum
        abuild -r -K -P /home/builder/packages
      '

      APK_FILE=\$(find /home/builder/packages -name \"${PKG_NAME}-${VERSION}-r${PKGREL}.apk\" -type f | head -n 1)
      cp \"\$APK_FILE\" \"/out/${PKG_NAME}-${VERSION}-r${PKGREL}-${arch}.apk\"

      # Export the exact standalone static musl binary from the built package
      if [ -f \"/home/builder/pkg/pkg/${PKG_NAME}/usr/bin/${PKG_NAME}\" ]; then
        cp \"/home/builder/pkg/pkg/${PKG_NAME}/usr/bin/${PKG_NAME}\" \"/out/${PKG_NAME}-linux-${arch}-musl\"
        chmod 0755 \"/out/${PKG_NAME}-linux-${arch}-musl\"
        file \"/out/${PKG_NAME}-linux-${arch}-musl\" || true
        ldd \"/out/${PKG_NAME}-linux-${arch}-musl\" 2>&1 || true
      fi

      echo '==> Verifying APK install and runtime dependencies:'
      apk add --allow-untrusted \"\$APK_FILE\"
      /usr/bin/${PKG_NAME} --version
      apk info -R ${PKG_NAME}
    "

  echo "==> Built: ${OUT_DIR}/${PKG_NAME}-${VERSION}-r${PKGREL}-${arch}.apk"
}

if ! command -v docker >/dev/null 2>&1; then
  echo "Error: docker is required to cross-build Alpine APKs from non-Alpine hosts." >&2
  echo "On Alpine Linux directly, build with: cd packaging/alpine && abuild -r" >&2
  exit 1
fi

for arch in "${ARCHES[@]}"; do
  build_in_docker "$arch"
done

(
  cd "$OUT_DIR"
  sha256sum ${PKG_NAME}-${VERSION}-r${PKGREL}-*.apk ${PKG_NAME}-linux-*-musl 2>/dev/null > SHA256SUMS || true
  cat SHA256SUMS
)
