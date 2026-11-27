# The docs

| Read | When you want to |
|---|---|
| [GUIDE.md](GUIDE.md) | learn Relentless from the start: install, agents, verifiers, sequences, state and memory, the interfaces, the CLI |
| [PROCESS.md](PROCESS.md) | write a process file: phases, step kinds, checks and reviews, ladders, the task force, re-entry, scenarios |
| [CONFIG.md](CONFIG.md) | look up a key of `rlt.yaml`, a verifier's rules, isolation, the reports and their schemas |
| [RUST_API.md](RUST_API.md) | drive a workflow from Rust: the driver, resume, human asks |
| [BACKENDS.md](BACKENDS.md) | add a coding agent: the contract, the capability flags, fixtures, tests |
| [INSTALL.md](INSTALL.md) | install a binary, build from source, pin a version |
| [CONCEPTS.md](CONCEPTS.md) | understand the ideas: the Ralph loop, Loop Engineering, Graph Engineering, Agentic Workflows as Code |
| [schemas/](schemas/) | the JSON Schemas of the run report, the live status, answers and the wiki lint |

Examples live in [`examples/`](../examples/): `hello/` (a first process, runnable without tokens), `hello-loop/` (the
smallest YAML loop), `process/` (a full process file with its scripts and prompts), `quality-gates/`, `p-vs-np/` and
the Rust drivers.
