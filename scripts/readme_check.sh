#!/bin/sh
# Runs every fenced block of README.md that follows a `<!-- check -->` line, in a fresh local clone of this repository,
# with the built `rlt` first on PATH; fails on the first block that exits non-zero. Run it by hand in every release:
#   scripts/readme_check.sh [path/to/rlt]
set -eu
cd "$(dirname "$0")/.."
BIN="${1:-$PWD/target/debug/rlt}"
[ -x "$BIN" ] || { echo "no rlt binary at $BIN (build it, or pass its path)" >&2; exit 2; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"   # the symlink below must not be relative
work=$(mktemp -d "${TMPDIR:-/tmp}/rlt-readme.XXXXXX")
trap 'rm -rf "$work"' EXIT
git clone -q . "$work/repo"
mkdir "$work/bin" "$work/home" && ln -s "$BIN" "$work/bin/rlt"
python3 - "$work" <<'PY'
import re, subprocess, sys

work = sys.argv[1]
text = open(f"{work}/repo/README.md").read()
blocks = re.findall(r"<!-- check -->\n```(?:bash|sh)\n(.*?)```", text, re.S)
if not blocks:
    sys.exit("README.md has no <!-- check --> block")
env = {"PATH": f"{work}/bin:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin", "HOME": f"{work}/home",
       # a clean HOME has no git identity, and a run commits
       "GIT_AUTHOR_NAME": "readme", "GIT_AUTHOR_EMAIL": "readme@example.com",
       "GIT_COMMITTER_NAME": "readme", "GIT_COMMITTER_EMAIL": "readme@example.com"}
for i, block in enumerate(blocks, 1):
    print(f"--- block {i}: {block.strip().splitlines()[0]}")
    r = subprocess.run(["sh", "-e", "-c", block], cwd=f"{work}/repo", env=env, text=True, capture_output=True)
    print(r.stdout[-600:], r.stderr[-600:])
    if r.returncode:
        sys.exit(f"block {i} failed with exit {r.returncode}")
print(f"{len(blocks)} block(s) ran")
PY
