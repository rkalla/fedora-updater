#!/usr/bin/env bash
# Install the single multi-call binary + polkit policy (requires root).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-/usr}"
BIN_DIR="${PREFIX}/bin"
POLICY_DIR="${PREFIX}/share/polkit-1/actions"

if [[ "${EUID}" -ne 0 ]]; then
  echo "Re-running with pkexec…"
  exec pkexec env PREFIX="$PREFIX" bash "$0" "$@"
fi

cargo build --release --bin fedora-updater --manifest-path "$ROOT/Cargo.toml"

install -d "$BIN_DIR" "$POLICY_DIR"
install -m 755 "$ROOT/target/release/fedora-updater" "$BIN_DIR/fedora-updater"
install -m 644 "$ROOT/data/dev.fedora.Updater.policy" "$POLICY_DIR/dev.fedora.Updater.policy"

# Clean up old split-helper install if present
rm -f "${PREFIX}/libexec/fedora-updater-helper"

echo "Installed:"
echo "  $BIN_DIR/fedora-updater          (GUI + --helper)"
echo "  $POLICY_DIR/dev.fedora.Updater.policy"
echo "Polkit action: dev.fedora.Updater.helper → $BIN_DIR/fedora-updater"
