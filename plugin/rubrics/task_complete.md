# Task Completion Rubric

A worker was asked to complete a task. From the CONTEXT (diff, logs, files), judge
whether the task is GENUINELY done — not just attempted.

Score 0–100:
- Does the change actually implement what was asked (50)?
- Is it correct / does it build & pass its checks (30)?
- Is it complete (no obvious TODOs, stubs, or "left for later") (20)?

A score >= 90 means done. Be skeptical: a plausible-looking but incomplete change is NOT done.

Output ONLY the verdict JSON: {"met": <bool>, "value": <0-100>, "max": 100, "target": 90, "rationale": "<one sentence>"}
