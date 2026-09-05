//! The one coarse operation over a package directory: parse its manifest and
//! refuse everything a marketplace package may not be.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use toml::Value;

use crate::manifest::ManifestError;
use crate::names::{CredentialPrefix, Namespace, PackageName, RelativePath};
use crate::package::{MANIFEST_FILE, Package, Role};
use crate::tree::{self, TreeDigestError};

/// Why a directory is not a package. Every variant names the file it read.
#[derive(Debug, Error)]
pub enum PackageError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("{root} is not a distributable tree: {source}")]
    Tree {
        root: PathBuf,
        #[source]
        source: TreeDigestError,
    },
    #[error("{manifest} declares `{field}` as `{declared}`, which is not in the package: {source}")]
    MissingPath {
        manifest: PathBuf,
        field: &'static str,
        declared: String,
        #[source]
        source: io::Error,
    },
    #[error("{manifest} declares `{field}` as `{declared}`, which is not {wanted}")]
    WrongKind {
        manifest: PathBuf,
        field: &'static str,
        declared: String,
        wanted: Kind,
    },
    #[error("{manifest} declares `{field}` as `{declared}`, which resolves outside the package")]
    EscapingPath {
        manifest: PathBuf,
        field: &'static str,
        declared: String,
    },
    #[error("cannot read {policy}: {source}")]
    PolicyRead {
        policy: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{policy} is not valid TOML: {source}")]
    PolicySyntax {
        policy: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{policy} declares `{field}`, which a deployment reads only from its own root config")]
    PolicyTopLevel { policy: PathBuf, field: String },
    #[error("{policy} declares `policy.{field}`, which an included fragment does not carry")]
    PolicyField { policy: PathBuf, field: String },
    #[error("{policy} declares a `policy.{kind}` without a name")]
    PolicyUnnamedDeclaration { policy: PathBuf, kind: &'static str },
    #[error("{policy} declares `include`, which only a deployment root declares")]
    PolicyInclude { policy: PathBuf },
    #[error("{policy} declares `externals.{key}`, which only a deployment root declares")]
    PolicyRootSetting { policy: PathBuf, key: String },
    #[error("{policy}: `{external}.command` is not `python3` and one of this package's helpers")]
    PolicyExternalCommand { policy: PathBuf, external: String },
    #[error("{policy}: `{external}.token_env` reads `{variable}`, which is outside `{prefix}`")]
    PolicyForeignCredential {
        policy: PathBuf,
        external: String,
        variable: String,
        prefix: CredentialPrefix,
    },
    #[error("{policy} declares `{contract}`, which is outside the namespaces it covers ({namespaces})")]
    PolicyForeignContract {
        policy: PathBuf,
        contract: String,
        namespaces: String,
    },
}

/// Read `<dir>/appa-package.toml` and refuse a package that is not
/// distributable: a tree with a symlink or beyond the source caps, a declared
/// path that is absent or escapes, or a battery policy that reaches outside its
/// own package.
pub fn validate_package(dir: &Path) -> Result<Package, PackageError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let package = Package::read(&manifest_path)?;

    // Symlinks and the source caps are refused by the same walk the digest
    // uses, so a package that validates can be digested and shipped.
    tree::walk(dir).map_err(|source| PackageError::Tree {
        root: dir.to_path_buf(),
        source,
    })?;

    let contained = Contained::new(dir, &manifest_path);
    match &package.role {
        Role::Battery(battery) => {
            let policy = contained.resolve(&battery.policy, "battery.policy", Kind::File)?;
            for helper in &battery.helpers {
                contained.resolve(helper, "battery.helpers", Kind::File)?;
            }
            check_policy(&policy, &package.name, &battery.namespaces, &battery.helpers)?;
        }
        Role::Adapter(adapter) => {
            contained.resolve(adapter.default_policy(), "adapter.default_policy", Kind::File)?;
            if let crate::package::Adapter::ClaudeCode { plugin_dir, .. } = adapter {
                contained.resolve(plugin_dir, "adapter.plugin_dir", Kind::Directory)?;
            }
        }
    }
    Ok(package)
}

/// Resolves a declared path against one package root.
struct Contained {
    root: PathBuf,
    manifest: PathBuf,
}

impl Contained {
    fn new(dir: &Path, manifest: &Path) -> Self {
        Self {
            // A package root reached through a symlinked parent (a temporary
            // directory, commonly) still resolves against its own real path.
            root: dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
            manifest: manifest.to_path_buf(),
        }
    }

    fn resolve(&self, declared: &RelativePath, field: &'static str, kind: Kind) -> Result<PathBuf, PackageError> {
        let resolved =
            self.root
                .join(declared.as_path())
                .canonicalize()
                .map_err(|source| PackageError::MissingPath {
                    manifest: self.manifest.clone(),
                    field,
                    declared: declared.to_string(),
                    source,
                })?;
        if !resolved.starts_with(&self.root) {
            return Err(PackageError::EscapingPath {
                manifest: self.manifest.clone(),
                field,
                declared: declared.to_string(),
            });
        }
        // A field names one kind of thing. A helper that is a directory and a
        // plugin tree that is a file both pass containment and fail at use, so
        // the package is refused here rather than at someone's deployment.
        let found = match resolved.is_dir() {
            true => Kind::Directory,
            false => Kind::File,
        };
        match found == kind {
            true => Ok(resolved),
            false => Err(PackageError::WrongKind {
                manifest: self.manifest.clone(),
                field,
                declared: declared.to_string(),
                wanted: kind,
            }),
        }
    }
}

/// What a declared path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::File => "a file",
            Self::Directory => "a directory",
        })
    }
}

// ---------------------------------------------------------------------------
// The battery's policy
// ---------------------------------------------------------------------------

/// The top-level tables an included fragment may carry, and the `[policy]`
/// fields it may declare. Both sets are the config loader's (`compose_include`):
/// a package that validates here must load there, so a fragment refused at
/// someone's deployment is refused at the package instead.
const INCLUDABLE_TABLES: [&str; 2] = ["policy", "externals"];
const INCLUDABLE_POLICY_FIELDS: [&str; 5] = ["version", "tool", "annotator", "authority", "sanitizer"];
/// Those of them that are arrays of named declarations.
const DECLARATION_ARRAYS: [&str; 4] = ["tool", "annotator", "authority", "sanitizer"];

/// A battery is a fragment a deployment includes, not a deployment: it neither
/// includes further files nor sets the root-only externals, it runs only its own
/// declared helpers, and it names only contracts in the namespaces it declares.
fn check_policy(
    policy: &Path,
    name: &PackageName,
    namespaces: &[Namespace],
    helpers: &[RelativePath],
) -> Result<(), PackageError> {
    let text = std::fs::read_to_string(policy).map_err(|source| PackageError::PolicyRead {
        policy: policy.to_path_buf(),
        source,
    })?;
    let document: Value = toml::from_str(&text).map_err(|source| PackageError::PolicySyntax {
        policy: policy.to_path_buf(),
        source,
    })?;

    // Named on its own because it is the mistake a battery author makes: a
    // battery is included, and cannot include in turn.
    if document.get("include").is_some() {
        return Err(PackageError::PolicyInclude {
            policy: policy.to_path_buf(),
        });
    }
    let top_level = document.as_table().ok_or_else(|| PackageError::PolicyTopLevel {
        policy: policy.to_path_buf(),
        field: "the document is not a table".to_owned(),
    })?;
    for field in top_level.keys() {
        if !INCLUDABLE_TABLES.contains(&field.as_str()) {
            return Err(PackageError::PolicyTopLevel {
                policy: policy.to_path_buf(),
                field: field.clone(),
            });
        }
    }
    // The loader reads the fragment's version to check it against the root's.
    // Which root it will meet is not knowable here; that it declares a version
    // at all, as an integer, is.
    let declared = top_level
        .get("policy")
        .and_then(Value::as_table)
        .ok_or_else(|| PackageError::PolicyTopLevel {
            policy: policy.to_path_buf(),
            field: "policy".to_owned(),
        })?;
    if declared.get("version").and_then(Value::as_integer).is_none() {
        return Err(PackageError::PolicyField {
            policy: policy.to_path_buf(),
            field: "version".to_owned(),
        });
    }
    for (field, value) in declared {
        if !INCLUDABLE_POLICY_FIELDS.contains(&field.as_str()) {
            return Err(PackageError::PolicyField {
                policy: policy.to_path_buf(),
                field: field.clone(),
            });
        }
        // The loader appends each of these to the root's own array.
        if field != "version" && !value.is_array() {
            return Err(PackageError::PolicyField {
                policy: policy.to_path_buf(),
                field: field.clone(),
            });
        }
    }

    if let Some(externals) = document.get("externals") {
        check_externals(policy, externals, name, helpers)?;
    }

    // A contract may carry an argument filter — `mcp/ns/send(channel:C1)` — so
    // the check is on the family and namespace it opens with, not on the whole
    // id: parsing the tool half is the registry's job, not this one's.
    let covered: Vec<String> = namespaces
        .iter()
        .flat_map(|namespace| ["mcp", "host", "agent"].map(|family| format!("{family}/{namespace}/")))
        .collect();
    check_named(policy, &document)?;
    for contract in declared_contracts(&document) {
        if !covered.iter().any(|prefix| contract.starts_with(prefix)) {
            return Err(PackageError::PolicyForeignContract {
                policy: policy.to_path_buf(),
                contract,
                namespaces: namespaces
                    .iter()
                    .map(Namespace::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }
    Ok(())
}

fn check_externals(
    policy: &Path,
    externals: &Value,
    name: &PackageName,
    helpers: &[RelativePath],
) -> Result<(), PackageError> {
    let root_setting = |key: &str| PackageError::PolicyRootSetting {
        policy: policy.to_path_buf(),
        key: key.to_owned(),
    };
    let Some(table) = externals.as_table() else {
        return Err(root_setting(""));
    };
    for (kind, value) in table {
        // A deployment's own settings (`timeout_ms`, `max_body_bytes`, …) sit
        // directly under `[externals]`, and so do the sections only a root
        // config may bind (`llm`, `claude_code`). A battery binds externals of
        // the kinds an included file may carry, and nothing else.
        if !BINDABLE_KINDS.contains(&kind.as_str()) {
            return Err(root_setting(kind));
        }
        let Some(bindings) = value.as_table() else {
            return Err(root_setting(kind));
        };
        for (id, binding) in bindings {
            check_binding(policy, binding, &format!("{kind}.{id}"), name, helpers)?;
        }
    }
    Ok(())
}

/// The external kinds an included file may bind. A battery is an included
/// fragment, so this is exactly the set the config loader accepts from one.
const BINDABLE_KINDS: [&str; 5] = ["authorities", "sanitizers", "annotators", "audience", "identity"];

/// One binding of one external. A battery runs the programs it ships and
/// nothing else: the `command` shape naming a declared helper is the only one
/// it may bind. The `url` shape would reach the network from inside a fragment
/// the deployment merely included, and the `builtin` shape would name a runtime
/// module the deployment did not choose — both are the root's to bind.
fn check_binding(
    policy: &Path,
    binding: &Value,
    external: &str,
    name: &PackageName,
    helpers: &[RelativePath],
) -> Result<(), PackageError> {
    let prefix = name.credential_prefix();
    let refuse = || PackageError::PolicyExternalCommand {
        policy: policy.to_path_buf(),
        external: external.to_owned(),
    };
    let Some(table) = binding.as_table() else {
        return Err(refuse());
    };
    let mut runs_a_helper = false;
    for (key, value) in table {
        match key.as_str() {
            "command" if runs_a_declared_helper(value, helpers) => runs_a_helper = true,
            // The runtime injects this variable into the helper process, so
            // whatever a package names here it receives. A package names only
            // the credentials it owns: outside its own prefix it would read
            // another package's, and outside the provider namespace entirely it
            // would read the deployment's own environment and fail to load.
            "token_env" if value.as_str().is_some_and(|variable| prefix.owns(variable)) => {}
            "token_env" => {
                return Err(PackageError::PolicyForeignCredential {
                    policy: policy.to_path_buf(),
                    external: external.to_owned(),
                    variable: value.as_str().unwrap_or_default().to_owned(),
                    prefix,
                });
            }
            _ => return Err(refuse()),
        }
    }
    match runs_a_helper {
        true => Ok(()),
        false => Err(refuse()),
    }
}

/// A battery ships the programs it runs, so an argv is exactly `python3` and one
/// of the helpers this manifest declares.
fn runs_a_declared_helper(command: &Value, helpers: &[RelativePath]) -> bool {
    let Some(argv) = command.as_array() else {
        return false;
    };
    match argv.iter().map(Value::as_str).collect::<Option<Vec<_>>>().as_deref() {
        Some(["python3", helper]) => helpers.iter().any(|declared| declared.as_str() == *helper),
        _ => false,
    }
}

/// Every declaration in the fragment carries a name.
///
/// A declaration whose `name` is missing or is not a string is an error rather
/// than an entry to skip: the loader appends it verbatim and its own typed read
/// then fails, so passing over it here would validate a battery that cannot
/// load. This says nothing about what the names are — an annotator or an
/// authority is named by an identifier, and only a tool is named by a contract.
fn check_named(policy: &Path, document: &Value) -> Result<(), PackageError> {
    for kind in DECLARATION_ARRAYS {
        for declaration in declarations(document, kind) {
            if declaration.get("name").and_then(Value::as_str).is_none() {
                return Err(PackageError::PolicyUnnamedDeclaration {
                    policy: policy.to_path_buf(),
                    kind,
                });
            }
        }
    }
    Ok(())
}

/// The tool contract names a policy declares. Only `[[policy.tool]]` names a
/// contract; the other declaration arrays name annotators, authorities and
/// sanitizers, which are identifiers and belong to no namespace.
fn declared_contracts(document: &Value) -> Vec<String> {
    declarations(document, "tool")
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// One kind of declaration array under the `[policy]` table every includable
/// fragment writes them in.
fn declarations<'a>(document: &'a Value, kind: &str) -> impl Iterator<Item = &'a Value> {
    document
        .get("policy")
        .and_then(|policy| policy.get(kind))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::names::Host;
    use crate::package::Adapter;

    const BATTERY_MANIFEST: &str = "schema = 1\nname = \"github\"\ndescription = \"GitHub MCP server\"\n\n\
         [battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n";

    const BATTERY_POLICY: &str = "[policy]\nversion = 2\n\n\
         [[policy.tool]]\nname = \"mcp/github/get_me\"\ndelta = {}\n\n\
         [externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n";

    /// A battery package on disk, with the policy body the caller wants.
    fn battery(policy: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("appa-package.toml"), BATTERY_MANIFEST).unwrap();
        fs::write(directory.path().join("appa.toml"), policy).unwrap();
        fs::write(directory.path().join("audience-source.py"), "print('{}')\n").unwrap();
        directory
    }

    fn claude_code_adapter() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appa-package.toml"),
            "schema = 1\nname = \"claude-code\"\ndescription = \"Claude Code adapter\"\n\n\
             [adapter]\nhost = \"claude-code\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n\
             plugin_dir = \"plugin\"\nplugin = \"appa-runtime\"\n",
        )
        .unwrap();
        fs::write(directory.path().join("default.appa.toml"), "[policy]\nversion = 2\n").unwrap();
        fs::create_dir(directory.path().join("plugin")).unwrap();
        fs::write(directory.path().join("plugin/plugin.json"), "{}\n").unwrap();
        directory
    }

    fn kagent_adapter() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appa-package.toml"),
            "schema = 1\nname = \"kagent\"\ndescription = \"kagent adapter\"\n\n\
             [adapter]\nhost = \"kagent\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n\
             images = { adk = \"ghcr.io/x/adk@sha256:aa\" }\n",
        )
        .unwrap();
        fs::write(directory.path().join("default.appa.toml"), "[policy]\nversion = 2\n").unwrap();
        directory
    }

    #[test]
    fn a_battery_package_validates() {
        let directory = battery(BATTERY_POLICY);

        let package = validate_package(directory.path()).unwrap();

        assert_eq!(package.name.as_str(), "github");
        assert_eq!(package.battery().unwrap().hosts, vec![Host::ClaudeCode]);
    }

    #[test]
    fn an_adapter_package_of_each_host_validates() {
        let claude_code = claude_code_adapter();
        let kagent = kagent_adapter();

        assert!(matches!(
            validate_package(claude_code.path()).unwrap().adapter(),
            Some(Adapter::ClaudeCode { .. })
        ));
        assert!(matches!(
            validate_package(kagent.path()).unwrap().adapter(),
            Some(Adapter::Kagent { .. })
        ));
    }

    #[test]
    fn a_missing_manifest_is_refused() {
        let directory = tempfile::tempdir().unwrap();

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::Manifest(ManifestError::Read { .. }))
        ));
    }

    #[test]
    fn a_symlink_in_the_tree_is_refused() {
        let directory = battery(BATTERY_POLICY);
        std::os::unix::fs::symlink("/etc/passwd", directory.path().join("passwd")).unwrap();

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::Tree { .. })
        ));
    }

    #[test]
    fn a_declared_path_that_is_absent_is_refused() {
        let directory = battery(BATTERY_POLICY);
        fs::remove_file(directory.path().join("audience-source.py")).unwrap();

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::MissingPath {
                field: "battery.helpers",
                ..
            })
        ));
    }

    #[test]
    fn a_plugin_directory_that_is_absent_is_refused() {
        let directory = claude_code_adapter();
        fs::remove_dir_all(directory.path().join("plugin")).unwrap();

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::MissingPath {
                field: "adapter.plugin_dir",
                ..
            })
        ));
    }

    #[test]
    fn a_policy_that_includes_another_file_is_refused() {
        let directory = battery(&format!("include = [\"batteries/other/appa.toml\"]\n{BATTERY_POLICY}"));

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::PolicyInclude { .. })
        ));
    }

    #[test]
    fn a_policy_that_sets_a_root_only_external_is_refused() {
        let directory = battery(&format!("{BATTERY_POLICY}\n[externals]\ntimeout_ms = 5000\n"));

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::PolicyRootSetting { .. })
        ));
    }

    #[test]
    fn a_policy_that_runs_an_undeclared_program_is_refused() {
        for command in [
            "[\"python3\", \"../other/audience-source.py\"]",
            "[\"bash\", \"audience-source.py\"]",
            "[\"python3\", \"audience-source.py\", \"--now\"]",
            "\"python3 audience-source.py\"",
        ] {
            let policy = BATTERY_POLICY.replace("[\"python3\", \"audience-source.py\"]", command);
            let directory = battery(&policy);

            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyExternalCommand { .. })
                ),
                "accepted {command}"
            );
        }
    }

    /// A battery runs the programs it ships. Every other binding shape reaches
    /// something the deployment did not choose — a network endpoint, a runtime
    /// module — from inside a fragment it merely included, so the shape itself
    /// is refused rather than its contents inspected.
    #[test]
    fn a_battery_binding_that_is_not_its_own_helper_is_refused() {
        let binding = "[externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n";
        for replacement in [
            "[externals.audience.github]\nurl = \"https://elsewhere.example/audience\"\n",
            "[externals.audience.github]\nbuiltin = \"llm\"\n",
            "[externals.authorities.review]\nurl = \"https://elsewhere.example/review\"\n",
            // A command beside a url is still a url binding on the wire.
            "[externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n             url = \"https://elsewhere.example/audience\"\n",
            // An external kind only a root config binds.
            "[externals.llm]\nprovider = \"anthropic\"\nmodel = \"claude-opus-5\"\n",
        ] {
            let directory = battery(&BATTERY_POLICY.replace(binding, replacement));

            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyExternalCommand { .. } | PackageError::PolicyRootSetting { .. })
                ),
                "accepted {replacement}"
            );
        }
    }

    /// The runtime injects the named variable into the helper it spawns, so a
    /// package that could name any variable could read any other package's
    /// credential. It names only the credentials its own name owns: the test
    /// package is `github`, so `APPA_PROVIDER_GITHUB` and its continuations.
    #[test]
    fn a_helper_reads_only_the_credentials_its_own_package_owns() {
        let named = |var: &str| {
            battery(&BATTERY_POLICY.replace(
                "command = [\"python3\", \"audience-source.py\"]\n",
                &format!("command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"{var}\"\n"),
            ))
        };

        for var in ["APPA_PROVIDER_GITHUB_TOKEN", "APPA_PROVIDER_GITHUB"] {
            let allowed = named(var);
            validate_package(allowed.path()).unwrap_or_else(|error| panic!("refused own credential {var}: {error}"));
        }

        for var in [
            // Another package's, and the deployment's own environment.
            "APPA_PROVIDER_SLACK_TOKEN",
            "APPA_GITHUB_TOKEN",
            "GITHUB_TOKEN",
            "",
            // A prefix is a whole name, not a character run: this belongs to
            // the package named `githubbot`, if one exists.
            "APPA_PROVIDER_GITHUBBOT_TOKEN",
        ] {
            let refused = named(var);
            assert!(
                matches!(
                    validate_package(refused.path()),
                    Err(PackageError::PolicyForeignCredential { .. })
                ),
                "accepted token_env {var:?}"
            );
        }
    }

    /// A declared path names one kind of thing. Containment alone accepts a
    /// helper that is a directory and a plugin tree that is a file: both resolve
    /// inside the package and both fail at the deployment that trusted them.
    #[test]
    fn a_declared_path_of_the_wrong_kind_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("appa-package.toml"), BATTERY_MANIFEST).unwrap();
        fs::write(directory.path().join("appa.toml"), BATTERY_POLICY).unwrap();
        fs::create_dir(directory.path().join("audience-source.py")).unwrap();

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::WrongKind {
                field: "battery.helpers",
                ..
            })
        ));

        let adapter = tempfile::tempdir().unwrap();
        fs::write(
            adapter.path().join("appa-package.toml"),
            "schema = 1\nname = \"claude-code\"\ndescription = \"Claude Code adapter\"\n\n\
             [adapter]\nhost = \"claude-code\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n\
             plugin_dir = \"plugin\"\nplugin = \"appa-runtime\"\n",
        )
        .unwrap();
        fs::write(adapter.path().join("default.appa.toml"), "[policy]\nversion = 2\n").unwrap();
        fs::write(adapter.path().join("plugin"), "not a tree").unwrap();

        assert!(matches!(
            validate_package(adapter.path()),
            Err(PackageError::WrongKind {
                field: "adapter.plugin_dir",
                ..
            })
        ));
    }

    /// A package that validates must load. Each of these is a fragment the
    /// config loader refuses when a deployment includes it, so the package is
    /// refused first, where the author can still see it.
    #[test]
    fn a_fragment_the_loader_would_refuse_is_refused_here() {
        let head = "[policy]\nversion = 2\n\n";
        for (body, what) in [
            (
                "[deployment]\nname = \"mine\"\n\n",
                "a top-level table only a root carries",
            ),
            (
                "[policy.audience]\ncustomer = []\n\n",
                "a policy field only a root carries",
            ),
            ("[policy.identity]\nme = []\n\n", "another root-only policy field"),
        ] {
            let directory = battery(&BATTERY_POLICY.replace(head, &format!("{head}{body}")));
            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyTopLevel { .. } | PackageError::PolicyField { .. })
                ),
                "accepted {what}"
            );
        }

        for version in ["", "version = \"2\"\n"] {
            let directory = battery(&BATTERY_POLICY.replace("version = 2\n", version));
            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyField { .. } | PackageError::PolicyTopLevel { .. })
                ),
                "accepted a fragment whose version is {version:?}"
            );
        }
    }

    /// A declaration without a name is not an entry to skip past. The loader
    /// appends it verbatim and its own typed read then fails, so a battery that
    /// validated here would refuse to load at whoever included it.
    #[test]
    fn a_declaration_without_a_name_is_refused() {
        for declaration in [
            "[[policy.tool]]\ndelta = {}\n",
            "[[policy.tool]]\nname = 7\ndelta = {}\n",
            "[[policy.annotator]]\ndelta = {}\n",
            "[[policy.sanitizer]]\nname = true\n",
        ] {
            let directory = battery(&format!("{BATTERY_POLICY}\n{declaration}"));

            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyUnnamedDeclaration { .. })
                ),
                "accepted {declaration}"
            );
        }
    }

    #[test]
    fn a_policy_that_names_another_packages_contract_is_refused() {
        let directory = battery(&BATTERY_POLICY.replace("mcp/github/get_me", "mcp/slack/post_message"));

        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::PolicyForeignContract { .. })
        ));
    }

    #[test]
    fn a_battery_may_cover_its_own_host_contracts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appa-package.toml"),
            "schema = 1\nname = \"claude-code\"\ndescription = \"Claude Code battery\"\n\n\
             [battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("appa.toml"),
            "[policy]\nversion = 2\n\n[[policy.tool]]\nname = \"host/claude-code/Bash(command:*.env*)\"\ndelta = {}\n",
        )
        .unwrap();

        assert!(validate_package(directory.path()).is_ok());
    }
}
