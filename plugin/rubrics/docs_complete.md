# Documentation Completeness Rubric

Assess whether the documentation in the CONTEXT adequately covers the project.

Score 0–100:
- Public API / commands documented (40): can a new user understand how to use it?
- Setup / install instructions (25): clear, runnable steps?
- Examples (20): at least one concrete worked example?
- Accuracy (15): does the doc match the actual code (no stale claims)?

A score >= 85 means the goal is met.

Base the score ONLY on what's in the CONTEXT. Be concise.
Output ONLY the verdict JSON: {"met": <bool>, "value": <0-100>, "max": 100, "target": 85, "rationale": "<one sentence>"}
