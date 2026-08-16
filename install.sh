#!/usr/bin/env bash
# Install a user-local GNOME launcher for dogfooding this checkout.
# Always points Exec at ./bin/fedora-updater in this project.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
APP_ID="dev.fedora.Updater"
APP_NAME="Fedora Updater"
BIN="${ROOT}/bin/fedora-updater"
ICON_SRC="${ROOT}/data/icons/${APP_ID}.png"

APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="${APP_DIR}/${APP_ID}.desktop"

# Common sizes GNOME Shell / GTK look for (missing 48/64 often falls back to generic)
ICON_SIZES=(16 24 32 48 64 96 128 256 512)

die() { echo "error: $*" >&2; exit 1; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

resize_icon() {
  local src="$1" size="$2" dest="$3"
  if command -v magick >/dev/null 2>&1; then
    magick "$src" -resize "${size}x${size}" "$dest"
  elif command -v convert >/dev/null 2>&1; then
    convert "$src" -resize "${size}x${size}" "$dest"
  elif command -v python3 >/dev/null 2>&1; then
    python3 - "$src" "$size" "$dest" <<'PY'
import sys
from PIL import Image
src, size, dest = sys.argv[1], int(sys.argv[2]), sys.argv[3]
im = Image.open(src).convert("RGBA")
im = im.resize((size, size), Image.Resampling.LANCZOS)
im.save(dest, format="PNG")
PY
  else
    die "need ImageMagick (magick/convert) or python3+Pillow to resize icons"
  fi
}

[[ -x "$BIN" ]] || die "binary not found or not executable: $BIN
Build first, e.g.:  ./build"

[[ -f "$ICON_SRC" ]] || die "icon source missing: $ICON_SRC"

need_cmd install

mkdir -p "$APP_DIR"

# Install sized icons into the user hicolor theme
for size in "${ICON_SIZES[@]}"; do
  dest_dir="${ICON_BASE}/${size}x${size}/apps"
  mkdir -p "$dest_dir"
  resize_icon "$ICON_SRC" "$size" "${dest_dir}/${APP_ID}.png"
done

# Also keep a full-resolution copy under hicolor for high-DPI / absolute fallback
mkdir -p "${ICON_BASE}/512x512/apps"
# Prefer absolute Icon path — GNOME Shell is more reliable with this for
# user-local dogfood installs than a themed name alone (avoids generic gear).
ICON_ABS="${ICON_BASE}/512x512/apps/${APP_ID}.png"
[[ -f "$ICON_ABS" ]] || die "failed to install icon at $ICON_ABS"

# Desktop entry: absolute Exec + Icon so each rebuild under ./bin is used
# and the custom artwork always resolves.
cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=${APP_NAME}
Comment=System, Flatpak, and firmware updates (dev build)
Exec=${BIN}
Icon=${ICON_ABS}
Terminal=false
Categories=System;PackageManager;
StartupNotify=true
StartupWMClass=${APP_ID}
Path=${ROOT}
EOF

# Refresh caches so the overview picks up name + icon
if command -v gtk4-update-icon-cache >/dev/null 2>&1; then
  gtk4-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
elif command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi

# Nudge GNOME / xdg to re-read the entry (icon was often cached as missing)
touch "$DESKTOP_FILE"

if command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "$DESKTOP_FILE" || true
fi

echo "Installed user-local launcher:"
echo "  Desktop: $DESKTOP_FILE"
echo "  Exec:    $BIN"
echo "  Icon:    $ICON_ABS"
echo "  Sizes:   ${ICON_SIZES[*]}"
echo
echo "Search Activities for “${APP_NAME}”."
echo "If the old generic icon is still cached, log out and back in"
echo "(on X11: Alt+F2 → r → Enter also works)."
