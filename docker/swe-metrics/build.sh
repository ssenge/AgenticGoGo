#!/usr/bin/env bash
# Build the reference swe-metrics judge image from a swe-metrics git checkout.
#
#   docker/swe-metrics/build.sh <swe-metrics checkout> [tag]      # tag: agg-swe-metrics
#   SWE_METRICS_REF=v0.1 docker/swe-metrics/build.sh ~/src/swe-metrics
#
# The context is `git archive` of ONE commit (SWE_METRICS_REF, default HEAD) plus the Dockerfile:
# uncommitted changes and the checkout's own venv/caches never reach the image, and the commit is
# recorded in the image's `org.opencontainers.image.revision` label. Works with docker (with or
# without BuildKit) and podman (ENGINE=podman). Needs network: pip and the pinned tool downloads.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
src=${1:-}
tag=${2:-agg-swe-metrics}
ref=${SWE_METRICS_REF:-HEAD}
[ -n "$src" ] || { sed -n '2,10p' "$0" >&2; exit 2; }
engine=${ENGINE:-$(command -v docker || command -v podman || true)}
[ -n "$engine" ] || { echo "no container engine (docker or podman) on PATH" >&2; exit 2; }

commit=$(git -C "$src" rev-parse --verify "$ref^{commit}")
ctx=$(mktemp -d)
trap 'rm -rf "$ctx"' EXIT
mkdir "$ctx/swe-metrics"
git -C "$src" archive "$commit" | tar -x -C "$ctx/swe-metrics"
cp "$here/Dockerfile" "$here/userinfo.cjs" "$ctx/"
echo "building $tag from swe-metrics $commit"
"$engine" build --build-arg "SWE_METRICS_COMMIT=$commit" -t "$tag" "$ctx"
