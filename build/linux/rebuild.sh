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
docker build -q -t freebank-app-build -f "$ROOT/build/linux/Dockerfile" "$ROOT/build/linux" >/dev/null
mkdir -p "$OUT"
docker run --rm -e SOURCE_DATE_EPOCH="$EPOCH" -e CARGO_BUILD_JOBS="${JOBS:-2}" -e BUNDLES="${BUNDLES:-deb}" \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$WORK":/src:ro -v "$OUT":/out \
    freebank-app-build
echo "Built ${SHA:0:12} in $OUT"
