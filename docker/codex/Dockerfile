# Reference image: OpenAI Codex as an agg worker (and LLM-judge ruler) under `isolation: container`.
#
#   docker build -t agg-codex docker/codex
#   # agg.yaml:  defaults: { agent: "codex", isolation: "container", image: "agg-codex" }
#
# It follows agg's IMAGE CONTRACT (docs/CONFIG.md → "The container tier runs the agent"):
#   - the agent CLI on PATH for ANY uid — agg runs the container as the project's owner, never root;
#   - /bin/sh, plus whatever your worker's tasks and your judges run (add your toolchain below);
#   - NO credentials: agg passes them at run time, by name (CODEX_API_KEY, OPENAI_API_KEY) or as a
#     read-only mount of ~/.codex/auth.json — never bake a key or a login into an image;
#   - no ENTRYPOINT (or one that execs its arguments): agg runs `codex <args…>` as the command;
#   - HOME is agg's to set (a fresh one per worker session) — nothing may rely on a passwd entry.
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git ripgrep \
 && rm -rf /var/lib/apt/lists/*

ARG CODEX_VERSION=latest
RUN npm install -g "@openai/codex@${CODEX_VERSION}" \
 && npm cache clean --force

CMD ["codex", "--version"]
