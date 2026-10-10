# A TEST image for the container tier: alpine plus a FAKE `claude` (a shell script that speaks
# Claude Code's stream-json and one-shot JSON). It follows the image contract the reference images
# in docker/ do — the CLI on PATH for any uid, /bin/sh, no credentials, no ENTRYPOINT — so the
# automated tests exercise the real runtime (mounts, HOME, credentials by name, read-only project
# for judges) without a model, a network or an account. Built offline from the local alpine:3.20.
FROM alpine:3.20
COPY claude /usr/local/bin/claude
RUN chmod 0755 /usr/local/bin/claude
