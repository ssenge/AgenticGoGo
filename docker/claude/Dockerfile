# Reference image: Claude Code as an agg worker (and LLM-judge ruler) under `isolation: container`.
#
#   docker build -t agg-claude docker/claude
#   # agg.yaml:  defaults: { agent: "claude", isolation: "container", image: "agg-claude" }
#
# It follows agg's IMAGE CONTRACT (docs/CONFIG.md → "The container tier runs the agent"):
#   - the agent CLI on PATH for ANY uid — agg runs the container as the project's owner, never root;
#   - /bin/sh, plus whatever your worker's tasks and your judges run (add your toolchain below);
#   - NO credentials: agg passes them at run time, by name (ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN)
#     or as read-only mounts — never bake a key or a login into an image;
#   - no ENTRYPOINT (or one that execs its arguments): agg runs `claude <args…>` as the command;
#   - HOME is agg's to set (a fresh one per worker session) — nothing may rely on a passwd entry.
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git ripgrep \
 && rm -rf /var/lib/apt/lists/*

ARG CLAUDE_CODE_VERSION=latest
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
 && npm cache clean --force

# A non-root uid cannot update a system-wide install; pin the version by rebuilding the image.
ENV DISABLE_AUTOUPDATER=1

CMD ["claude", "--version"]
