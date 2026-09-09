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

## Install and manage a deployment

Start with an APPA release binary and Claude Code installed:

```sh
appa plugin install claude-code
appa battery install github
appa plugin list
appa battery list
```

The first install uses that binary's published generation. A generation binds
the catalog, packages, runtime, native plugin and image descriptors to one commit.
Source commits without published artifacts cannot be installed online. Subsequent
installs retain the selected generation; nothing updates automatically.

Claude installation registers its native plugin and verifies the running APPA
runtime. Battery installation adds and activates its policy in the same operation.
It does not register an MCP server or obtain credentials. A connection with a
different identity can be associated using `--server <connection-id>`; use the
identity reported by the host's discovery/validation, not a guessed provider URL.

Use `--config <path>` to select another deployment. An explicit
`appa plugin install claude-code --revision <full-commit>` updates the whole
selected generation. A previously retained commit can be restored the same way.
`appa battery remove github` removes unchanged installer-owned configuration;
`appa plugin remove claude-code` unregisters owned Claude support. Removal keeps
authored configuration, retained artifacts, trajectory data and the runtime.
Changed owned files are preserved and reported for recovery, not overwritten.

Commands never prompt. `--json` emits one result or error on stdout, while
progress goes to stderr. Exit codes are 0 for success, 1 for failure, 2 for
invalid command syntax and 3 when recovery is required. Offline transfer uses
the shared `appa bundle` command described below; import restores the bundle's
selection rather than merging it with the destination's selection.

## Prepare kagent

```sh
appa plugin install kagent --config ./deployment/appa.toml
appa battery install github --config ./deployment/appa.toml
```

Installation prepares local Helm values, Agent snippets, an image lock and an
operator-invoked image verifier. It never applies resources to a Kubernetes
context. The result names a private directory under the deployment's `.appa/`
state. Read its `KAGENT.md` and `CONFIGURATION.txt` before deploying.

Both Python and Go are prepared by default. Use `--runtime python` or `--runtime
go` when only one is needed; subsequent installs retain that choice. Go requires
Linux amd64 nodes. Settings target kagent 0.9.12's native declarative agents;
the controller image setting affects all its ordinary declarative agents.

The runtime image is digest-pinned. Agent images use generation-specific tags;
the verifier compares registry and running-image digests to the lock. This is
verification, not Kubernetes enforcement: a mismatched agent image can start
before a post-deployment check detects it.

Small configuration trees are embedded in the generated ConfigMap values.
The complete `assets/` tree is always present. Larger trees or empty command
directories use an operator-populated read-only PVC; preparation explains that
prerequisite. ConfigMaps are not secret storage. Supply credentials separately.

Battery changes regenerate the prepared deployment. Reapply it explicitly to
change a cluster. `appa plugin remove kagent --config ./deployment/appa.toml`
removes the selected prepared files, preserving authored policy, retained
packages, trajectory data and live cluster resources. Export/import use the same
bundle command as Claude; container images and cluster credentials are separate
prerequisites. No automatic updates occur.

The separately published `appa-kagent-demo` chart is a demonstration, not an
installed generation artifact. Its release checksum is in `SHA256SUMS`; the
marketplace installs and bundles only the runtime chart.

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

Export checks the completed archive against the importer's extraction limits
before publishing it. The output filesystem needs temporary space for the
compressed archive and up to 512 MiB of extracted content. A bundle whose combined
payload exceeds the import limit is refused even if it compresses below that limit.

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
