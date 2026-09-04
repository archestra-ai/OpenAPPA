# The demo tools MCP server: one file over streamable HTTP, canned data.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM python:3.13-slim@sha256:9d2e5553305c7c7b0097999bb17187c69b921ccd6bc9d40e4bb5ebe652c00285
RUN pip install --no-cache-dir "mcp>=1.25,<2"
COPY demo_tools.py /srv/demo_tools.py
EXPOSE 3000
ENTRYPOINT ["python", "/srv/demo_tools.py", "--host", "0.0.0.0", "--port", "3000"]
