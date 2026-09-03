# The demo mock externals: stdlib-only, one file, no dependencies.
FROM python:3.13-slim
COPY mock_externals.py /srv/mock_externals.py
EXPOSE 8081
ENTRYPOINT ["python", "/srv/mock_externals.py", "--host", "0.0.0.0", "--port", "8081"]
