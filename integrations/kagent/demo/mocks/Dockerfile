# The demo mock externals: stdlib-only, one file, no dependencies.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM dhi.io/python:3.13.18-debian13-dev@sha256:02e72cc2c83c22c06fcb4cbe23c54edb339e248bee4774ac640fabac635529a6
COPY --chown=65532:65532 mock_externals.py /srv/mock_externals.py
USER 65532:65532
EXPOSE 8081
ENTRYPOINT ["python", "/srv/mock_externals.py", "--host", "0.0.0.0", "--port", "8081"]
