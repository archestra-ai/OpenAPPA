# The demo mock externals: stdlib-only, one file, no dependencies.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM python:3.13-slim@sha256:9d2e5553305c7c7b0097999bb17187c69b921ccd6bc9d40e4bb5ebe652c00285
RUN groupadd --system --gid 65532 appa \
    && useradd --system --uid 65532 --gid 65532 --no-create-home appa
COPY --chown=appa:appa mock_externals.py /srv/mock_externals.py
USER 65532:65532
EXPOSE 8081
ENTRYPOINT ["python", "/srv/mock_externals.py", "--host", "0.0.0.0", "--port", "8081"]
