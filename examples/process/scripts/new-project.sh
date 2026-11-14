#!/bin/sh
# Makes the project "$1" from the new-project form (AGG_FIELD_TASK, AGG_FIELD_SIZE): a git repo with its task and roles.
set -e
mkdir -p "$1" && cd "$1"
git init -q -b main
git config user.email agg@example.invalid && git config user.name agg
printf '%s\n' "$AGG_FIELD_TASK" > task.md
printf 'worker: { agent: claude, model: fake }\ncritic: { agent: codex }\n' > tools.yaml
: > settings.yaml
: > critic.ok
git add -A && git commit -qm "the project's inputs"
