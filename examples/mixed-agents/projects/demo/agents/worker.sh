#!/bin/sh
# A stand-in for any agent CLI: it gets the brief's file as $1 and works in the project. A real CLI (Gemini CLI,
# OpenCode, Qwen Code, Pi, or one of your own) is described the same way under `agents:` in process.yaml.
echo "reading the brief in $1"
printf 'print(1 + 2)\n' > add.py
git add add.py && git -c user.name=worker -c user.email=worker@example.com commit -qm "fix add.py"
echo "fixed add.py"
