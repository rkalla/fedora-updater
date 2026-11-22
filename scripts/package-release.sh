#!/usr/bin/env bash
# Package Fedora Updater release artifacts (RPM + portable tarball + SHA256SUMS)
# into ./dist/.
#
# Usage:
#   ./scripts/package-release.sh [--version <version>] [--skip-build]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION=""
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      if [[ $# -lt 2 ]]; then
        echo "error: --version requires a value" >&2
        exit 2
      fi
      VERSION="$2"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    -h|--help)
      sed -n '2,6p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
fi
VERSION="${VERSION#v}"

if [[ -z "$VERSION" ]]; then
  echo "error: could not determine version from Cargo.toml" >&2
  exit 1
fi

command -v rpmbuild >/dev/null 2>&1 || {
  echo "error: rpmbuild not found (sudo dnf install rpm-build)" >&2
  exit 1
}

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  "$ROOT/build"
fi

BIN="$ROOT/bin/fedora-updater"
if [[ ! -x "$BIN" ]]; then
  echo "error: release binary missing or not executable: $BIN" >&2
  exit 1
fi

ARCH="$(uname -m)"
DIST_DIR="$ROOT/dist"
mkdir -p "$DIST_DIR"

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

# 1. Portable tar.gz archive
TAR_NAME="fedora-updater-${VERSION}-linux-${ARCH}"
STAGE_DIR="$TMP_DIR/$TAR_NAME"
install -d "$STAGE_DIR/bin" "$STAGE_DIR/data/icons"
install -m 755 "$BIN" "$STAGE_DIR/bin/fedora-updater"
install -m 644 "$ROOT/data/dev.fedora.Updater.desktop" "$STAGE_DIR/data/dev.fedora.Updater.desktop"
install -m 644 "$ROOT/data/dev.fedora.Updater.policy" "$STAGE_DIR/data/dev.fedora.Updater.policy"
install -m 644 "$ROOT/data/icons/dev.fedora.Updater.png" "$STAGE_DIR/data/icons/dev.fedora.Updater.png"
install -m 755 "$ROOT/install.sh" "$STAGE_DIR/install.sh"
install -m 755 "$ROOT/uninstall.sh" "$STAGE_DIR/uninstall.sh"
install -m 644 "$ROOT/README.md" "$STAGE_DIR/README.md"
install -m 644 "$ROOT/LICENSE" "$STAGE_DIR/LICENSE"

TAR_FILE="${TAR_NAME}.tar.gz"
tar -C "$TMP_DIR" -czf "$DIST_DIR/$TAR_FILE" "$TAR_NAME"

# 2. RPM package
RPM_TOP="$TMP_DIR/rpmbuild"
mkdir -p "$RPM_TOP"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

rpmbuild -bb \
  --define "_topdir $RPM_TOP" \
  --define "src_root $ROOT" \
  --define "pkg_version $VERSION" \
  "$ROOT/packaging/fedora-updater.spec"

BUILT_RPM="$(find "$RPM_TOP/RPMS" -type f -name '*.rpm' | head -n1)"
if [[ -z "$BUILT_RPM" ]]; then
  echo "error: rpmbuild did not produce an RPM package" >&2
  exit 1
fi

RPM_FILE="$(basename "$BUILT_RPM")"
install -m 644 "$BUILT_RPM" "$DIST_DIR/$RPM_FILE"

# 3. SHA256 checksums
(
  cd "$DIST_DIR"
  sha256sum "$RPM_FILE" "$TAR_FILE" > SHA256SUMS.txt
)

echo
echo "Packaged release artifacts in $DIST_DIR:"
ls -lh "$DIST_DIR/$RPM_FILE" "$DIST_DIR/$TAR_FILE" "$DIST_DIR/SHA256SUMS.txt"
