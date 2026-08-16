#!/usr/bin/env bash
# Remove the user-local GNOME launcher installed by ./install.sh
set -euo pipefail

APP_ID="dev.fedora.Updater"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="${APP_DIR}/${APP_ID}.desktop"

removed=0

if [[ -f "$DESKTOP_FILE" ]]; then
  rm -f "$DESKTOP_FILE"
  echo "Removed $DESKTOP_FILE"
  removed=1
else
  echo "No desktop file at $DESKTOP_FILE"
fi

# Remove all installed size variants of the app icon
shopt -s nullglob
icon_paths=("${ICON_BASE}"/*/apps/"${APP_ID}".png)
if ((${#icon_paths[@]})); then
  for p in "${icon_paths[@]}"; do
    rm -f "$p"
    echo "Removed $p"
    removed=1
  done
else
  echo "No icons matching ${ICON_BASE}/*/apps/${APP_ID}.png"
fi
shopt -u nullglob

if command -v gtk4-update-icon-cache >/dev/null 2>&1; then
  gtk4-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
elif command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi

if [[ "$removed" -eq 1 ]]; then
  echo
  echo "Uninstalled user-local “Fedora Updater” launcher."
  echo "If the icon still appears in Activities, log out and back in."
else
  echo
  echo "Nothing to remove."
fi
