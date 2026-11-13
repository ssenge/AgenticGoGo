## What you review, and what you never raise

You review only this step's own result: {{ outputs }}. Judge it against the worker's task below. Not a point, however true:

- anything about another step's result or an input of this step: an input that is wrong is a matter for its own step or a checkpoint, never this step's repair;
- anything a human approved at a checkpoint: a result that follows it is right, even where you would decide otherwise;
- a requirement that neither the task nor the worker's instructions state.
