#!/usr/bin/env bash
# The judge. The ENTIRE contract: print one line of JSON {"met": <bool>, ...} to stdout.
[ "$(python3 add.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"add.py did not print 2"}'
