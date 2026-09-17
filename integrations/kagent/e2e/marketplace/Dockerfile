# Isolated acceptance fixture; no provider credentials and no GitHub network calls.
FROM python:3.13-slim@sha256:9d2e5553305c7c7b0097999bb17187c69b921ccd6bc9d40e4bb5ebe652c00285
RUN pip install --no-cache-dir "mcp>=1.25,<2" \
    && groupadd --system --gid 65532 appa \
    && useradd --system --uid 65532 --gid 65532 --no-create-home appa
COPY --chown=appa:appa fixtures.py /srv/fixtures.py
USER 65532:65532
EXPOSE 8080 3000
ENTRYPOINT ["python", "/srv/fixtures.py"]
