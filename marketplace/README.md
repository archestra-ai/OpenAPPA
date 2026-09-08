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

## Custom files in offline bundles

Official battery policies and helpers are bundled automatically. For your own
scripts and data, list individual files in the root deployment config:

```toml
include = ["../shared/policy.toml"]

[bundle]
files = ["helpers/check.py", "helpers/rules.json", "../shared/helper.py"]
```

Paths are relative to that config. Manual policy includes are copied automatically;
list their helper scripts and data explicitly. Parent-relative source paths are
supported. Directories, wildcards, symlinks and special files are not bundled.
APPA copies only the listed files and included configs, never neighboring files.
Custom files and their metadata are limited to 64 MiB and 4096 tree entries.

Use the existing `appa bundle --output ./deployment.tar.gz` command. Import with
`appa plugin install claude-code --from ./deployment.tar.gz --sha256 <trusted-digest>`.
The archive contains configuration and custom files: keep it private.

Import preserves relative layouts and command working directories in a verified
snapshot under the deployment's `.appa/` directory. It does not overwrite sibling
files, rewrite command arguments, install interpreters or libraries, or obtain
credentials. Absolute command arguments remain deployment-specific prerequisites.
Unix executable intent is preserved; files imported from Windows have no POSIX
executable bit, so invoke such scripts through their interpreter on Unix.

Re-export carries the verified imported snapshot again. Editing imported snapshot
files or their generated references is an error, not an implicit update. To change
custom dependencies, edit the authored source config/files, export a new bundle,
and import it explicitly. Ordinary policy edits and battery changes do not require
repackaging unchanged custom files.
