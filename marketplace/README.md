# APPA marketplace

The marketplace distributes two package kinds:

- **Plugins** install APPA support for an agent host. A plugin package contains
  host-specific plugin files or image references and default configuration.
- **Batteries** supply provider policies and declared helpers.

The smaller protocol-translation components remain **adapters** in the runtime
API. They are implementation components of the host plugins, not package kinds.

Each directory under `plugins/` or `batteries/` has an `appa-package.toml`.
The directory name must match the manifest's package name. The package manifest
is the authored source; `marketplace.toml` is generated from these manifests and
the package contents.

After adding or editing a package, run:

```sh
bash scripts/appa-marketplace.sh
```

Commit the resulting catalog with the package changes. The script includes
current tracked and non-ignored files, including new packages, and excludes
ignored test artifacts. It validates packages and ownership before replacing
the catalog. It uses the same digest implementation as the package library.

CI runs `bash scripts/appa-marketplace.sh --check` and fails when the catalog
needs regeneration. Regeneration is a developer operation; it does not update
installed deployments or contact a remote marketplace.
