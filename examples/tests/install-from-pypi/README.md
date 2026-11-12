# install-from-pypi

A Python package install reaches two hosts: `pypi.org` serves the index, and
`files.pythonhosted.org` serves the files. One rule covers both, because `|`
separates the alternatives of a selector pattern.

1. Reading a file under `/hr/` limits the data to `hr@archestra.ai`.
2. An install whose `hosts` are all PyPI's runs with no label effect.
3. An install that names any other host requires public data.

- `install-from-pypi.appa`: an install to `evil.example` is allowed before the
  read. After the read, installs to both PyPI hosts, in either order, and to
  `pypi.org` alone are allowed. Installs are denied when they also name
  `evil.example`, or name `upload.pypi.org`, `pypi.org.evil.example`, the
  literal text `pypi.org|files.pythonhosted.org`, or no host.
