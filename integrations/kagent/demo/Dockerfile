# The demo tools MCP server: one file over streamable HTTP, canned data.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM dhi.io/python:3.13.18-debian13-dev@sha256:02e72cc2c83c22c06fcb4cbe23c54edb339e248bee4774ac640fabac635529a6
RUN python -m venv /opt/appa-venv \
    && /opt/appa-venv/bin/pip install --no-cache-dir "mcp>=1.25,<2"
ENV PATH=/opt/appa-venv/bin:$PATH
COPY --chown=65532:65532 demo_tools.py /srv/demo_tools.py
USER 65532:65532
EXPOSE 3000
ENTRYPOINT ["python", "/srv/demo_tools.py", "--host", "0.0.0.0", "--port", "3000"]
