#!/usr/bin/env bash
# Build every release artifact into dist/: AppImage (+ .zsync), .deb, .rpm and .tar.gz.
# Usage: packaging/build-release.sh <path-to-appimagetool>
# Needs: dpkg-deb, cargo-generate-rpm (cargo install cargo-generate-rpm).
set -euo pipefail
cd "$(dirname "$0")/.."
TOOL=${1:?path to appimagetool}
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
rm -rf dist && mkdir -p dist
cargo build --release -p sublimers
strip target/release/sublimers || true

packaging/build-appimage.sh "$TOOL" dist
packaging/build-deb.sh && mv "target/sublimers_${VERSION}_amd64.deb" dist/
cargo generate-rpm -p crates/app -o "dist/sublimers-${VERSION}-1.x86_64.rpm"

TAR=sublimers-${VERSION}-x86_64-linux
mkdir -p "target/$TAR"
cp target/release/sublimers packaging/sublimers.desktop packaging/sublimers-512.png README.md LICENSE-MIT LICENSE-APACHE "target/$TAR/"
tar -C target -czf "dist/$TAR.tar.gz" "$TAR"

(cd dist && sha256sum -- * > SHA256SUMS)
ls -la dist
