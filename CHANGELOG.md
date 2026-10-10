# Changelog

## Unreleased

### Added
- **Judges built on swe-metrics** (A-20). A judge can be DECLARED in `judges:` instead of written as a
  file: `metric: { id, tool?, stat?, labels?, scope?, combine? }` reads one number of a
  [swe-metrics](https://github.com/ssenge/swe-metrics) report, `score: { definition, version }` its
  0–100 quality score, each held to `max:`/`min:`.
  - All metric and score judges of a session share ONE `swe-metrics measure` of the session's tree; the
    report and the tools' SARIF are kept as evidence.
  - Fail closed: a skipped or failed analyzer, or an absent metric, makes the judge error with the
    analyzer's reason — never met.
  - Several tools for one metric: `tool:` or `combine: strictest | all | majority`; with `all`, tools
    moving in different directions are no improvement for the ratchet (the verdict's new optional
    `mixed` field).
  - `scope: changed` measures only the files changed since the base branch; a sub-path scope measures a
    directory of the project.
  - A score is computed against the incumbent's report (`agg/private/metrics/incumbent/`); a score
    capped by a failed gate names the gates; a definition/version mismatch is refused; a `null` score is
    an error with the score's notes; a changed definition resets the ratchet reference on `--resume`.
  - A ratchet on a metric or score judge may omit `direction` (the judge states it; the report's
    direction is checked against it).
  - swe-metrics stays optional: a config declaring such a judge is refused at startup when
    `swe-metrics` is not on `PATH` or speaks another schema version.
  - Rust driver: `Judge::metric(name, MetricJudge::new(id)…)` and `Judge::score(name, ScoreJudge::new(def, version)…)`.

### Changed
- `sequence.ratchet.<judge>.direction` is optional for metric and score judges (still required for
  every other judge — refused at startup without it, now located at `sequence.ratchet.<judge>.direction`).
- `judges.<name>` accepts `metric`, `score`, `max`, `min` beside `timeout`.
