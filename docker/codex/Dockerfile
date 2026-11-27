# Reference image: OpenAI Codex as an rlt worker (and rubric verifier ruler) under `isolation: container`.
#
#   docker build -t rlt-codex docker/codex
#   # rlt.yaml:  defaults: { agent: "codex", isolation: "container", image: "rlt-codex" }
#
# It follows rlt's IMAGE CONTRACT (docs/CONFIG.md → "The container tier runs the agent"):
#   - the agent CLI on PATH for ANY uid — rlt runs the container as the project's owner, never root;
#   - /bin/sh, plus whatever your worker's tasks and your verifiers run (add your toolchain below);
#   - NO credentials: rlt passes them at run time, by name (CODEX_API_KEY, OPENAI_API_KEY) or as a
#     read-only mount of ~/.codex/auth.json — never bake a key or a login into an image;
#   - no ENTRYPOINT (or one that execs its arguments): rlt runs `codex <args…>` as the command;
#   - HOME is rlt's to set (a fresh one per worker session) — nothing may rely on a passwd entry.
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git ripgrep \
 && rm -rf /var/lib/apt/lists/*

ARG CODEX_VERSION=latest
RUN npm install -g "@openai/codex@${CODEX_VERSION}" \
 && npm cache clean --force

CMD ["codex", "--version"]
