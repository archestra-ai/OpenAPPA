# appa-kagent-adk: the gated python runtime image for kagent.
#
# One layer over kagent's published runtime image: the appa-kagent-adk
# package and its entrypoint. The base stays as published — same venv,
# same user, same ports — and the entrypoint replays the stock startup
# with the OpenAPPA construction deltas.
#
#   docker build -t appa-kagent-adk integrations/kagent/appa-kagent-adk
#   (build context: this directory)
#
# The controller appends `--host <bind> --port 8080 --filepath /config`
# to the entrypoint, exactly as it does for the stock image.
#
# The package is a pure-python wheel built in a first stage: kagent's
# base image runs uv offline, so the final install takes a prebuilt
# wheel with --no-deps (the base venv already carries google-adk and
# httpx at the versions kagent locked) and needs no build backend and
# no network.

ARG UV_VERSION=0.11.15
ARG KAGENT_APP_IMAGE=cr.kagent.dev/kagent-dev/kagent/app:0.9.12

FROM python:3.13-slim AS wheel
COPY --from=ghcr.io/astral-sh/uv:0.11.15 /uv /usr/local/bin/uv
COPY . /pkg
RUN uv build --wheel /pkg --out-dir /dist

FROM ${KAGENT_APP_IMAGE}
COPY --from=wheel --chown=python:pythongroup /dist/ /tmp/appa-dist/
RUN uv pip install --python /.kagent/.venv/bin/python --no-deps --offline /tmp/appa-dist/*.whl

ENTRYPOINT ["appa-kagent-adk"]
