//! The one coarse operation over a package directory: parse its manifest and
//! refuse everything a marketplace package may not be.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use toml::Value;

use crate::manifest::ManifestError;
use crate::names::{Namespace, RelativePath};
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
    #[error("{policy} declares `include`, which only a deployment root declares")]
    PolicyInclude { policy: PathBuf },
    #[error("{policy} declares `externals.{key}`, which only a deployment root declares")]
    PolicyRootSetting { policy: PathBuf, key: String },
    #[error("{policy}: `{external}.command` is not `python3` and one of this package's helpers")]
    PolicyExternalCommand { policy: PathBuf, external: String },
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
            let policy = contained.resolve(&battery.policy, "battery.policy")?;
            for helper in &battery.helpers {
                contained.resolve(helper, "battery.helpers")?;
            }
            check_policy(&policy, &battery.namespaces, &battery.helpers)?;
        }
        Role::Adapter(adapter) => {
            contained.resolve(adapter.default_policy(), "adapter.default_policy")?;
            if let crate::package::Adapter::ClaudeCode { plugin_dir, .. } = adapter {
                contained.resolve(plugin_dir, "adapter.plugin_dir")?;
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

    fn resolve(&self, declared: &RelativePath, field: &'static str) -> Result<PathBuf, PackageError> {
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
        match resolved.starts_with(&self.root) {
            true => Ok(resolved),
            false => Err(PackageError::EscapingPath {
                manifest: self.manifest.clone(),
                field,
                declared: declared.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// The battery's policy
// ---------------------------------------------------------------------------

/// A battery is a fragment a deployment includes, not a deployment: it neither
/// includes further files nor sets the root-only externals, it runs only its own
/// declared helpers, and it names only contracts in the namespaces it declares.
fn check_policy(policy: &Path, namespaces: &[Namespace], helpers: &[RelativePath]) -> Result<(), PackageError> {
    let text = std::fs::read_to_string(policy).map_err(|source| PackageError::PolicyRead {
        policy: policy.to_path_buf(),
        source,
    })?;
    let document: Value = toml::from_str(&text).map_err(|source| PackageError::PolicySyntax {
        policy: policy.to_path_buf(),
        source,
    })?;

    if document.get("include").is_some() {
        return Err(PackageError::PolicyInclude {
            policy: policy.to_path_buf(),
        });
    }

    if let Some(externals) = document.get("externals") {
        check_externals(policy, externals, helpers)?;
    }

    // A contract may carry an argument filter — `mcp/ns/send(channel:C1)` — so
    // the check is on the family and namespace it opens with, not on the whole
    // id: parsing the tool half is the registry's job, not this one's.
    let covered: Vec<String> = namespaces
        .iter()
        .flat_map(|namespace| ["mcp", "host", "agent"].map(|family| format!("{family}/{namespace}/")))
        .collect();
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

fn check_externals(policy: &Path, externals: &Value, helpers: &[RelativePath]) -> Result<(), PackageError> {
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
            check_binding(policy, binding, &format!("{kind}.{id}"), helpers)?;
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
fn check_binding(policy: &Path, binding: &Value, external: &str, helpers: &[RelativePath]) -> Result<(), PackageError> {
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
            // A token the helper reads is the deployment's to name, but naming
            // one is inert on its own: the runtime refuses a variable outside
            // its own namespace when it reads the binding.
            "token_env" if value.is_str() => {}
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

/// The tool contract names a policy declares, under the `[policy]` table a
/// battery writes and at the top level a bare fragment would.
fn declared_contracts(document: &Value) -> Vec<String> {
    let arrays = [
        document.get("policy").and_then(|policy| policy.get("tool")),
        document.get("tool"),
    ];
    arrays
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
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

    /// The token a helper reads is named beside the command that runs it, and
    /// naming one binds nothing on its own.
    #[test]
    fn a_battery_may_name_the_token_its_own_helper_reads() {
        let directory = battery(&BATTERY_POLICY.replace(
            "command = [\"python3\", \"audience-source.py\"]\n",
            "command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"APPA_GITHUB_TOKEN\"\n",
        ));

        validate_package(directory.path()).expect("a helper may read a named token");
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
