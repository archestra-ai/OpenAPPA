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

FROM python:3.13-slim@sha256:e48f59cdba69a3cbd9ed2cf0f348d90d45a8d350d98134cf1070bc106ee9ed90 AS wheel
COPY --from=ghcr.io/astral-sh/uv:0.11.15@sha256:e590846f4776907b254ac0f44b5b380347af5d90d668138ca7938d1b0c2f98d3 /uv /usr/local/bin/uv
COPY . /pkg
RUN uv build --wheel /pkg --out-dir /dist

# CPython 3.13.18 for the base's uv-managed interpreter: kagent 0.9.12 ships
# 3.13.13, which lacks the ssl fix for CVE-2026-115557. The same
# python-build-standalone install_only build uv installs, digests from the
# 20261110 release's SHA256SUMS.
FROM python:3.13-slim@sha256:e48f59cdba69a3cbd9ed2cf0f348d90d45a8d350d98134cf1070bc106ee9ed90 AS cpython
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
      amd64) arch=x86_64 digest=50c774f6674740739f791c05a9177ed03e2fe02abebda9bca3512eb7309b37cc ;; \
      arm64) arch=aarch64 digest=d25a65036d5e149db902fa52ad65878d0c00e9301360ec197832f99063087e9e ;; \
      *) exit 1 ;; \
    esac \
 && python3 -c 'import hashlib, sys, urllib.request; data = urllib.request.urlopen(sys.argv[1]).read(); assert hashlib.sha256(data).hexdigest() == sys.argv[2]; open("/tmp/cpython.tar.gz", "wb").write(data)' \
      "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.13.18+20261110-${arch}-unknown-linux-gnu-install_only.tar.gz" "${digest}" \
 && mkdir -p "/out/cpython-3.13.18-linux-${arch}-gnu" \
 && tar -xzf /tmp/cpython.tar.gz -C "/out/cpython-3.13.18-linux-${arch}-gnu" --strip-components=1

FROM ${KAGENT_APP_IMAGE}
ARG TARGETARCH
# Fixes the base ships without: Wolfi curl and brotli past CVE-2026-19931 and
# CVE-2026-111497, and CPython 3.13.18. The new curl links OpenSSL 4.0, whose
# library package clashes with the openssl CLI package (both own
# /etc/ssl/openssl.cnf). Nothing depends on that CLI, kagent 0.10's base no
# longer ships it, and it is the only importer of the OpenSSL CMP client
# (CVE-2026-63073), so it goes. Both venvs reach Python through the
# /python/cpython-3.13-linux-<arch>-gnu link uv made, so repointing it moves
# them to the new interpreter; their packages are unchanged.
USER root
COPY --from=cpython --chown=python:pythongroup /out/ /python/
RUN apk del --no-cache openssl \
 && apk upgrade --no-cache curl libcurl-openssl4 libbrotlicommon1 libbrotlidec1 libbrotlienc1 \
 && case "${TARGETARCH}" in amd64) arch=x86_64 ;; arm64) arch=aarch64 ;; *) exit 1 ;; esac \
 && ln -sfn "/python/cpython-3.13.18-linux-${arch}-gnu" "/python/cpython-3.13-linux-${arch}-gnu" \
 && rm -rf "/python/cpython-3.13.13-linux-${arch}-gnu" \
 && sed -i 's/^version_info = .*/version_info = 3.13.18/' /.kagent/.venv/pyvenv.cfg /.kagent/sandbox-venv/pyvenv.cfg \
 && /.kagent/.venv/bin/python -c 'import sys, ssl, kagent.adk; assert sys.version_info[:3] == (3, 13, 18), sys.version' \
 && /.kagent/sandbox-venv/bin/python -c 'import sys; assert sys.version_info[:3] == (3, 13, 18), sys.version'
USER python
COPY --from=wheel --chown=python:pythongroup /dist/ /tmp/appa-dist/
RUN uv pip install --python /.kagent/.venv/bin/python --no-deps --offline /tmp/appa-dist/*.whl

ENTRYPOINT ["appa-kagent-adk"]
