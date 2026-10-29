#!/usr/bin/env bash
# Build Sublimers-x86_64.AppImage. Usage: packaging/build-appimage.sh <path-to-appimagetool> [out-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
TOOL=${1:?path to appimagetool}
OUT=${2:-target}
cargo build --release -p sublimers
APPDIR=target/Sublimers.AppDir
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/icons/hicolor/512x512/apps" "$APPDIR/usr/share/applications"
install -m755 target/release/sublimers "$APPDIR/usr/bin/sublimers"
strip "$APPDIR/usr/bin/sublimers" || true
install -m644 packaging/sublimers-512.png "$APPDIR/usr/share/icons/hicolor/512x512/apps/sublimers.png"
install -m644 packaging/sublimers-512.png "$APPDIR/sublimers.png"
ln -sf sublimers.png "$APPDIR/.DirIcon"
install -m644 packaging/sublimers.desktop "$APPDIR/sublimers.desktop"
install -m644 packaging/sublimers.desktop "$APPDIR/usr/share/applications/sublimers.desktop"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/sublimers" "$@"
RUN
chmod +x "$APPDIR/AppRun"
# Update info lets AppImageUpdate fetch deltas from the latest GitHub release.
UPDATE="gh-releases-zsync|genetzhs|sublimers|latest|Sublimers-x86_64.AppImage.zsync"
ARCH=x86_64 "$TOOL" --no-appstream -u "$UPDATE" "$APPDIR" "$OUT/Sublimers-x86_64.AppImage"
# appimagetool writes the .zsync into the current directory.
if [ -f Sublimers-x86_64.AppImage.zsync ] && [ "$OUT" != "." ]; then
    mv -f Sublimers-x86_64.AppImage.zsync "$OUT/"
fi
