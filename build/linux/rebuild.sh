#!/usr/bin/env bash
# rebuild.sh [commit or tag]: rebuild the FreeBank app's Linux program and .deb in the pinned image (Dockerfile here),
# from a commit of this repository, and print their sha256. Needs Docker. Output in build/linux/out/<commit>/.
# (If your docker's buildx plugin is too old for its daemon, run with DOCKER_BUILDKIT=0: the classic builder makes the
# same image.)
set -euo pipefail
REF=${1:-HEAD}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SHA=$(git -C "$ROOT" rev-parse --verify "$REF^{commit}")
EPOCH=$(git -C "$ROOT" log -1 --format=%ct "$SHA")
OUT=${OUT:-$ROOT/build/linux/out/${SHA:0:12}}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
git -C "$ROOT" archive "$SHA" | tar -x -C "$WORK"
# The image: the saved copy named in image.lock when it was made from this very Dockerfile (a release build then needn't
# reach Ubuntu's package snapshot, which was down for hours on 2026-11-05), else built from the Dockerfile. The copy's
# sha256 is checked before it is loaded; any failure falls back to the build.
image_from_lock() {
    local lock="$ROOT/build/linux/image.lock" url sha df
    [ -f "$lock" ] || return 1
    url=$(sed -n 's/^url=//p' "$lock"); sha=$(sed -n 's/^sha256=//p' "$lock"); df=$(sed -n 's/^dockerfile_sha256=//p' "$lock")
    [ -n "$url" ] && [ -n "$sha" ] || return 1
    [ "$df" = "$(sha256sum "$ROOT/build/linux/Dockerfile" | cut -d' ' -f1)" ] || { echo "image.lock is for another Dockerfile: building it" >&2; return 1; }
    curl -fsSL --retry 5 --retry-delay 10 -o "$WORK/image.tar.gz" "$url" || return 1
    echo "$sha  $WORK/image.tar.gz" | sha256sum -c --quiet - || return 1
    local id
    id=$(gunzip -c "$WORK/image.tar.gz" | docker load -q | sed -n 's/^Loaded image[^:]*: //p' | tail -1) || return 1
    [ -n "$id" ] && docker tag "$id" freebank-app-build && rm -f "$WORK/image.tar.gz" && echo "build image: the saved copy ($url)" >&2
}
image_from_lock || docker build -q -t freebank-app-build -f "$ROOT/build/linux/Dockerfile" "$ROOT/build/linux" >/dev/null
mkdir -p "$OUT"
docker run --rm -e SOURCE_DATE_EPOCH="$EPOCH" -e CARGO_BUILD_JOBS="${JOBS:-2}" -e BUNDLES="${BUNDLES:-deb}" \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$WORK":/src:ro -v "$OUT":/out \
    freebank-app-build
echo "Built ${SHA:0:12} in $OUT"
