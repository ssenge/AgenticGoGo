# Reference JUDGE image: swe-metrics and its pinned tools, for metric and score judges under
# `isolation: container` (`judge: { image: "agg-swe-metrics" }`).
#
#   docker/swe-metrics/build.sh ~/src/swe-metrics            # → agg-swe-metrics
#
# The build context is NOT this directory: build.sh assembles one from a swe-metrics git checkout
# (`git archive` of a commit — swe-metrics is installed from source) plus this Dockerfile, so the
# image is exactly one swe-metrics commit, recorded in its `org.opencontainers.image.revision` label.
#
# It follows agg's IMAGE CONTRACT (docs/CONFIG.md → "The container tier runs the agent"):
#   - `swe-metrics` and every tool it runs on PATH for ANY uid — agg runs the container as the
#     project's owner, never root — installed system-wide under /opt, not under /root;
#   - /bin/sh, git (`scope: changed`, the git history analyzer);
#   - no credentials, no ENTRYPOINT, nothing that relies on a passwd entry — HOME is agg's to set.
#
# `judge.image` is the image EVERY judge of the run uses: when the run also has script judges, add
# their toolchain below (or start your own judge image `FROM agg-swe-metrics`).
#
# trivy's vulnerability database is NOT baked in (it is ~1.5 GB and goes stale within a day): agg
# mounts the host's trivy cache read-only into the container (docs/CONFIG.md → metric judges).
FROM python:3.12-slim-bookworm

# what scripts/install-tools.sh needs (curl, tar, unzip, xz, python3 with venv) and git.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git unzip xz-utils \
 && rm -rf /var/lib/apt/lists/*

ARG SWE_METRICS_COMMIT=unknown
LABEL org.opencontainers.image.title="agg-swe-metrics" \
      org.opencontainers.image.revision="${SWE_METRICS_COMMIT}"

COPY swe-metrics /opt/swe-metrics/src

# swe-metrics itself, in its own venv (its Python dependencies are pinned in its pyproject.toml).
RUN python3 -m venv /opt/swe-metrics/venv \
 && /opt/swe-metrics/venv/bin/pip install --no-cache-dir --disable-pip-version-check /opt/swe-metrics/src \
 && ln -s /opt/swe-metrics/venv/bin/swe-metrics /usr/local/bin/swe-metrics

# every external tool, exactly as pinned (and checksum-verified) in the checkout's tools.lock.
RUN HOME=/tmp/build-home /opt/swe-metrics/src/scripts/install-tools.sh \
      --prefix /opt/swe-metrics/tools --bin-dir /usr/local/bin \
 && /opt/swe-metrics/src/scripts/install-tools.sh --prefix /opt/swe-metrics/tools --bin-dir /usr/local/bin --check \
 && rm -rf /tmp/build-home /root/.cache /root/.npm

# a uid with no passwd entry (agg runs as the project owner) must not crash a node tool.
COPY userinfo.cjs /opt/swe-metrics/userinfo.cjs

# a non-root uid cannot write the image's bytecode caches; nothing should try. And no tool phones
# home for a newer version or telemetry: a measurement must not depend on the network (behind agg's
# allow-list each such call is refused and logged as a denied connection).
ENV PYTHONDONTWRITEBYTECODE=1 \
    NODE_OPTIONS="--require /opt/swe-metrics/userinfo.cjs" \
    TFLINT_DISABLE_VERSION_CHECK=1 \
    CHECKPOINT_DISABLE=1 \
    TRIVY_SKIP_VERSION_CHECK=true \
    TRIVY_DISABLE_TELEMETRY=true \
    CKV_SKIP_PACKAGE_UPDATE_CHECK=true

CMD ["swe-metrics", "tools"]
