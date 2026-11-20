# The demo tools MCP server: one file over streamable HTTP, canned data.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM python:3.13-slim@sha256:e48f59cdba69a3cbd9ed2cf0f348d90d45a8d350d98134cf1070bc106ee9ed90
RUN pip install --no-cache-dir "mcp>=1.25,<2"
COPY demo_tools.py /srv/demo_tools.py
EXPOSE 3000
ENTRYPOINT ["python", "/srv/demo_tools.py", "--host", "0.0.0.0", "--port", "3000"]
