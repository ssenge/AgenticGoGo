#!/bin/sh
# A stand-in for an agent CLI that takes a moment. $1 is a pointer to the brief, which rlt writes to
# rlt/private/INSTRUCTIONS.md in the lane's worktree (the working folder); its `FILE=` line names what this lane writes.
file=$(sed -n 's/^FILE=//p' rlt/private/INSTRUCTIONS.md | head -1)
[ -n "$file" ] || { echo "ok"; exit 0; }   # rlt's start-up probe has no file: it only asks for an answer
start=$(date +%s)
sleep 2
printf '%s, written %s to %s\n' "$file" "$start" "$(date +%s)" > "$file"
git add "$file" && git -c user.name=worker -c user.email=worker@example.com commit -qm "write $file"
echo "wrote $file"
