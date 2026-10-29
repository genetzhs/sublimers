#!/usr/bin/env bash
# Build target/sublimers_<version>_amd64.deb. Usage: packaging/build-deb.sh
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cargo build --release -p sublimers
ROOT=target/deb/sublimers_${VERSION}_amd64
rm -rf "$ROOT"
install -Dm755 target/release/sublimers "$ROOT/usr/bin/sublimers"
strip "$ROOT/usr/bin/sublimers" || true
install -Dm644 packaging/sublimers.desktop "$ROOT/usr/share/applications/sublimers.desktop"
install -Dm644 packaging/sublimers-512.png "$ROOT/usr/share/icons/hicolor/512x512/apps/sublimers.png"
install -Dm644 packaging/sublimers.png "$ROOT/usr/share/icons/hicolor/256x256/apps/sublimers.png"
install -Dm644 packaging/sublimers.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/sublimers.svg"
install -Dm644 LICENSE-MIT "$ROOT/usr/share/doc/sublimers/LICENSE-MIT"
install -Dm644 LICENSE-APACHE "$ROOT/usr/share/doc/sublimers/LICENSE-APACHE"
SIZE=$(du -sk "$ROOT/usr" | cut -f1)
mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CTL
Package: sublimers
Version: $VERSION
Section: editors
Priority: optional
Architecture: amd64
Depends: libc6 (>= 2.35), libgl1, libxkbcommon0
Installed-Size: $SIZE
Maintainer: Genetzakis Manolis <genetzhs@gmail.com>
Homepage: https://github.com/genetzhs/sublimers
Description: Fast, Sublime Text-style code editor written in Rust
 Multi-cursor editing, Goto Anything, command palette, split view,
 code folding, syntax highlighting and hot exit.
CTL
dpkg-deb --root-owner-group --build "$ROOT" "target/sublimers_${VERSION}_amd64.deb"
