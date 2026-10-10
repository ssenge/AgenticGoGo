# quality-gates — judges you declare instead of write

Most quality gates measure software: complexity, duplication, secrets, vulnerabilities. With
[swe-metrics](https://github.com/ssenge/swe-metrics) installed, such a judge is a few lines of
`agg/agg.yaml` — no script that runs a tool and parses its output:

```yaml
judges:
  cc_max:  { metric: { id: "complexity.cyclomatic", stat: "max", tool: "lizard" }, max: 5 }
  quality: { score: { definition: "swe-default", version: "2" }, min: 70 }
```

`pricing.py` starts with one function of cyclomatic complexity 12, so `cc_max` is unmet and the worker
has something to refactor. Every session, agg runs **one** `swe-metrics measure` that all six judges
share, keeps the report and the tools' SARIF as evidence under `agg/private/evidence/`, scores the
session against the report of the last kept state, and rolls a session back if it makes `cc_p95` or
the score worse (the ratchet), or leaks a secret (the `secrets` invariant).

## Run it

```bash
cp -r examples/quality-gates /tmp/qg && cd /tmp/qg
git init -q -b main && printf 'agg/state/\nagg/private/\n' > .gitignore && git add -A && git commit -qm base
agg check              # refuses if swe-metrics is missing or incompatible
agg judge cc_max       # one judge, by hand: {"met":false,"value":12,…}
agg run
```

Details: [`docs/CONFIG.md` — judges built on swe-metrics](../../docs/CONFIG.md#judges-built-on-swe-metrics--metric-and-score).
