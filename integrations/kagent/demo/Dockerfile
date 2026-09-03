# The demo tools MCP server: one file over streamable HTTP, canned data.
FROM python:3.13-slim
RUN pip install --no-cache-dir "mcp>=1.25,<2"
COPY demo_tools.py /srv/demo_tools.py
EXPOSE 3000
ENTRYPOINT ["python", "/srv/demo_tools.py", "--host", "0.0.0.0", "--port", "3000"]
