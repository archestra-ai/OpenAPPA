# appa-kagent-adk: the OpenAPPA python runtime image for kagent.
#
# One layer over kagent's published runtime image: the appa-kagent-adk
# package and its entrypoint. The base stays as published — same venv,
# same user, same ports. `APPA_ENABLED` selects what the entrypoint
# serves. The image leaves it unset, so it serves the stock kagent
# startup. `APPA_ENABLED=true` replays that startup with the OpenAPPA
# construction deltas, and needs `APPA_RUNTIME_URL`.
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
#
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.

ARG KAGENT_APP_IMAGE=cr.kagent.dev/kagent-dev/kagent/app:0.9.12@sha256:5ee30b4584e8de3266eb3cc11f5c46e8627716339d04d14166c50bda5f0f4182

FROM python:3.13-slim@sha256:9d2e5553305c7c7b0097999bb17187c69b921ccd6bc9d40e4bb5ebe652c00285 AS wheel
COPY --from=ghcr.io/astral-sh/uv:0.11.15@sha256:e590846f4776907b254ac0f44b5b380347af5d90d668138ca7938d1b0c2f98d3 /uv /usr/local/bin/uv
COPY . /pkg
RUN uv build --wheel /pkg --out-dir /dist

FROM ${KAGENT_APP_IMAGE}
COPY --from=wheel --chown=python:pythongroup /dist/ /tmp/appa-dist/
RUN uv pip install --python /.kagent/.venv/bin/python --no-deps --offline /tmp/appa-dist/*.whl

ENTRYPOINT ["appa-kagent-adk"]
