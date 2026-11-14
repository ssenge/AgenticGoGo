#!/bin/sh
# The process's own facts on the run report (`ui.overlay`): called with the project and the report's kind, the report on
# stdin. Here: what the survey step is for, and how many steps the report has.
steps=$(grep -o '"id":"' | wc -l | tr -d ' ')
printf '{"steps": {"survey": {"does": "lists the units to build"}}, "run": {"kind": "%s", "ids": %s}}\n' "$2" "$steps"
