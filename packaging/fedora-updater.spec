%global debug_package %{nil}
%{!?src_root:%global src_root %(pwd)}

Name:           fedora-updater
Version:        %{?pkg_version}%{!?pkg_version:0.5.0}
Release:        1
Summary:        Unified GTK4/Libadwaita updater for Fedora system, Flatpak, and firmware updates

License:        Apache-2.0
URL:            https://github.com/rkalla/fedora-updater

Requires:       hicolor-icon-theme
Recommends:     polkit
Recommends:     dnf5daemon-server
Recommends:     fwupd
Recommends:     flatpak

%description
A fast, transparent GTK4 + Libadwaita desktop updater for Fedora that brings
system (DNF), Flatpak (user and system, including runtime extensions), and
LVFS firmware updates together in a single Observatory workspace.

Ships as a single multi-call binary (fedora-updater / fedora-updater --helper)
with Polkit privilege separation and zero-password update discovery.

%install
if [ ! -x "%{src_root}/bin/fedora-updater" ]; then
  echo "error: expected built binary at %{src_root}/bin/fedora-updater" >&2
  exit 1
fi

install -Dm755 "%{src_root}/bin/fedora-updater" "%{buildroot}%{_bindir}/fedora-updater"
install -Dm644 "%{src_root}/data/dev.fedora.Updater.policy" "%{buildroot}%{_datadir}/polkit-1/actions/dev.fedora.Updater.policy"
install -Dm644 "%{src_root}/data/dev.fedora.Updater.desktop" "%{buildroot}%{_datadir}/applications/dev.fedora.Updater.desktop"
install -Dm644 "%{src_root}/LICENSE" "%{buildroot}%{_licensedir}/%{name}/LICENSE"
install -Dm644 "%{src_root}/README.md" "%{buildroot}%{_pkgdocdir}/README.md"

for size in 16 24 32 48 64 96 128 256 512; do
  icon_dir="%{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps"
  install -d "$icon_dir"
  icon_dest="${icon_dir}/dev.fedora.Updater.png"
  if command -v python3 >/dev/null 2>&1 && python3 -c "import PIL" 2>/dev/null; then
    python3 - "%{src_root}/data/icons/dev.fedora.Updater.png" "$size" "$icon_dest" <<'PY'
import sys
from PIL import Image
src, size, dest = sys.argv[1], int(sys.argv[2]), sys.argv[3]
im = Image.open(src).convert("RGBA")
im = im.resize((size, size), Image.Resampling.LANCZOS)
im.save(dest, format="PNG")
PY
  elif command -v magick >/dev/null 2>&1; then
    magick "%{src_root}/data/icons/dev.fedora.Updater.png" -resize "${size}x${size}" "$icon_dest"
  elif command -v convert >/dev/null 2>&1; then
    convert "%{src_root}/data/icons/dev.fedora.Updater.png" -resize "${size}x${size}" "$icon_dest"
  else
    echo "error: need python3-pillow or ImageMagick (magick/convert) to resize icons" >&2
    exit 1
  fi
done

%check
desktop-file-validate "%{buildroot}%{_datadir}/applications/dev.fedora.Updater.desktop"

%files
%license %{_licensedir}/%{name}/LICENSE
%doc %{_pkgdocdir}/README.md
%{_bindir}/fedora-updater
%{_datadir}/applications/dev.fedora.Updater.desktop
%{_datadir}/polkit-1/actions/dev.fedora.Updater.policy
%{_datadir}/icons/hicolor/*/apps/dev.fedora.Updater.png
