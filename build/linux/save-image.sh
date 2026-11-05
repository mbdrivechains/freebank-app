#!/usr/bin/env bash
# save-image.sh <out dir>: save this machine's freebank-app-build image (built from build/linux/Dockerfile) as a
# gzipped docker archive, and print the lines for image.lock (the url is where the archive is then published).
# rebuild.sh loads that copy when its Dockerfile hash matches, so a release build needn't reach Ubuntu's snapshot.
set -euo pipefail
OUT=${1:?an output folder}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DF=$(sha256sum "$ROOT/build/linux/Dockerfile" | cut -d' ' -f1)
DOCKER_BUILDKIT=${DOCKER_BUILDKIT:-0} docker build -q -t freebank-app-build -f "$ROOT/build/linux/Dockerfile" "$ROOT/build/linux" >/dev/null
NAME="freebank-app-build-${DF:0:12}.tar.gz"
mkdir -p "$OUT"
docker save freebank-app-build | gzip -n > "$OUT/$NAME"
echo "dockerfile_sha256=$DF"
echo "sha256=$(sha256sum "$OUT/$NAME" | cut -d' ' -f1)"
echo "file=$OUT/$NAME ($(du -h "$OUT/$NAME" | cut -f1))"
