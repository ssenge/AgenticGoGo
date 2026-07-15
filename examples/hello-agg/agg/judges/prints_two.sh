#!/usr/bin/env bash
# The judge, resolved by the NAME `prints_two` in done_if. The ENTIRE contract: print one line
# of JSON {"met": <bool>, ...} to stdout. agg runs this from the project root; the agent never does.
[ "$(python3 add.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"add.py did not print 2"}'
