#!/bin/sh
# A stand-in for an agent CLI that takes a moment. $1 is a pointer to the brief, which rlt writes to
# rlt/private/INSTRUCTIONS.md in the lane's worktree (the working folder); its `ITEM=` line names the unit.
item=$(sed -n 's/^ITEM=//p' rlt/private/INSTRUCTIONS.md | head -1)
[ -n "$item" ] || { echo "ok"; exit 0; }   # rlt's start-up probe has no unit: it only asks for an answer
start=$(date +%s)
sleep 2
mkdir -p out
printf 'unit %s, worked %s to %s\n' "$item" "$start" "$(date +%s)" > "out/$item.txt"
git add out && git -c user.name=worker -c user.email=worker@example.com commit -qm "make $item"
echo "made $item"
