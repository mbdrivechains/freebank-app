#!/bin/sh
# Inside the image: build the app from /src (read-only) at the fixed path /build, and put the program and the .deb
# in /out. SOURCE_DATE_EPOCH is the commit's time (rebuild.sh sets it).
set -eu
: "${SOURCE_DATE_EPOCH:?the commit time}"
cp -a /src /build && cd /build
export CARGO_TERM_COLOR=never
npm ci --ignore-scripts --no-audit --no-fund  # no install scripts: none is needed for a build (v0.2.5)
# BUNDLES=deb,appimage also makes the AppImage. Its tools are downloaded at build time and set a library path in its
# copy of the program, so the AppImage isn't reproducible yet; the program and the .deb are.
npx tauri build --bundles "${BUNDLES:-deb}" -- --locked
cp src-tauri/target/release/freebank /out/
for f in src-tauri/target/release/bundle/appimage/*.AppImage; do [ -f "$f" ] && cp "$f" /out/; done
# The release's .deb, as the release workflow makes it (dpkg-deb stamps SOURCE_DATE_EPOCH).
scripts/deb-finalize.sh src-tauri/target/release/bundle/deb/*.deb /out >/dev/null
chown -R "${HOST_UID:-0}:${HOST_GID:-0}" /out
cd /out && sha256sum freebank *.deb $(ls *.AppImage 2>/dev/null)
