# Isolated acceptance fixture; no provider credentials and no GitHub network calls.
FROM python:3.13-slim@sha256:e48f59cdba69a3cbd9ed2cf0f348d90d45a8d350d98134cf1070bc106ee9ed90
RUN pip install --no-cache-dir "mcp>=1.25,<2" \
    && groupadd --system --gid 65532 appa \
    && useradd --system --uid 65532 --gid 65532 --no-create-home appa
COPY --chown=appa:appa fixtures.py /srv/fixtures.py
USER 65532:65532
EXPOSE 8080 3000
ENTRYPOINT ["python", "/srv/fixtures.py"]
