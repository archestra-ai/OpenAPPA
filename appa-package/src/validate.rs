//! The one coarse operation over a package directory: parse its manifest and
//! refuse everything a marketplace package may not be.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use toml::Value;

use crate::manifest::ManifestError;
use crate::names::{CredentialPrefix, Namespace, PackageName, RelativePath};
use crate::package::{BatteryTarget, MANIFEST_FILE, Package, Role};
use crate::tree::{self, EntryKind, TreeDigestError};

/// Why a directory is not a package. Every variant names the file it read.
#[derive(Debug, Error)]
pub enum PackageError {
    #[error("{policy}: invalid battery readiness metadata: {reason}")]
    Readiness { policy: PathBuf, reason: String },
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
        wanted: EntryKind,
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
    #[error("{policy} confines the results of `{tool}`, a tool it does not declare")]
    PolicyConfinesForeignTool { policy: PathBuf, tool: String },
    #[error("{policy} declares a `policy.{kind}` without a name")]
    PolicyUnnamedDeclaration { policy: PathBuf, kind: &'static str },
    #[error(
        "{policy} has an invalid server qualifier on `{contract}`; use a short MCP tool name and one server namespace"
    )]
    PolicyServer { policy: PathBuf, contract: String },
    #[error("{policy} declares `include`, which only a deployment root declares")]
    PolicyInclude { policy: PathBuf },
    #[error("{policy} declares `externals.{key}`, which only a deployment root declares")]
    PolicyRootSetting { policy: PathBuf, key: String },
    #[error("{policy}: `{external}.command` is not `python3` and one of this package's helpers")]
    PolicyExternalCommand { policy: PathBuf, external: String },
    #[error("{policy}: `externals.jev` carries one `token_env` and nothing else")]
    PolicyJevProfile { policy: PathBuf },
    #[error("{policy}: `{external}.token_env` reads `{variable}`, which is outside `{prefix}`")]
    PolicyForeignCredential {
        policy: PathBuf,
        external: String,
        variable: String,
        prefix: CredentialPrefix,
    },
    #[error("{policy} uses policy version {found}, but the battery's base uses {base}")]
    TargetPolicyVersion { policy: PathBuf, base: i64, found: i64 },
    #[error("{policy} declares `{external}`, which the battery's base already declares")]
    TargetPolicyDuplicateExternal { policy: PathBuf, external: NamedExternal },
    #[error("{policy} declares `{declaration}`, which the battery's base already declares")]
    TargetPolicyDuplicateDeclaration {
        policy: PathBuf,
        declaration: NamedDeclaration,
    },
    #[error("`{field}` names the battery's base policy, which a deployment of the target already includes")]
    TargetPolicyIsBase { field: &'static str },
    #[error("`{field}` is outside the package root, where a battery with targets keeps its policies")]
    TargetedPolicyOutsidePackageRoot { field: &'static str },
    #[error("{policy} is a targeted battery's base, which loads alone and so runs no helper, yet binds `{external}`")]
    BaseRunsHelper { policy: PathBuf, external: NamedExternal },
    #[error(
        "{policy} is a targeted battery's base, which loads alone and so names no audience source, yet names `{entry}`"
    )]
    BaseMentionsSource { policy: PathBuf, entry: String },
    #[error(
        "{policy} is a targeted battery's base, which loads alone, yet a tool names annotator `{annotator}` it does not declare"
    )]
    BaseUsesTargetAnnotator { policy: PathBuf, annotator: String },
    #[error(
        "{policy} is a targeted battery's base, which loads alone, yet binds no implementation for `{declaration}`"
    )]
    BaseDeclarationUnbound {
        policy: PathBuf,
        declaration: NamedDeclaration,
    },
    #[error("{policy} names `{tool}` without a bare variant, so a call outside its selectors falls through to `*`")]
    ToolWithoutBareVariant { policy: PathBuf, tool: String },
    #[error("{policy} is a targeted battery's base, which loads alone, yet binds `{external}` without declaring it")]
    BaseBindingUndeclared { policy: PathBuf, external: NamedExternal },
    #[error("{policy} declares `{variant}` after the bare `{tool}`, which matches first")]
    UnreachableVariant {
        policy: PathBuf,
        tool: String,
        variant: String,
    },
    #[error("{policy} declares a bare `{tool}`, which shadows the base's variants of it")]
    TargetShadowsBase { policy: PathBuf, tool: String },
    #[error("{policy} declares `{contract}`, which is outside the namespaces it covers ({namespaces})")]
    PolicyForeignContract {
        policy: PathBuf,
        contract: String,
        namespaces: String,
    },
    #[error("{policy}: `audience.{name}` sends its member lookups to itself")]
    AudienceLookupToItself { policy: PathBuf, name: String },
    #[error(
        "{policy}: `audience.{name}` sends its member lookups to `audience.{target}`, which sends its own elsewhere; a lookup takes one hop"
    )]
    ChainedAudienceLookup {
        policy: PathBuf,
        name: String,
        target: String,
    },
}

/// Read `<dir>/appa-package.toml` and refuse a package that is not
/// distributable: a tree with a symlink or beyond the source caps, a declared
/// path that is absent or escapes, or a battery policy that reaches outside its
/// own package.
pub fn validate_package(dir: &Path) -> Result<Package, PackageError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let mut package = Package::read(&manifest_path)?;

    // Symlinks and the source caps are refused by the same walk the digest
    // uses, so a package that validates can be digested and shipped.
    tree::walk(dir).map_err(|source| PackageError::Tree {
        root: dir.to_path_buf(),
        source,
    })?;

    let contained = Contained::new(dir, &manifest_path);
    match &mut package.role {
        Role::Battery(battery) => {
            let policy = contained.resolve(&battery.base, "battery.policy", EntryKind::File)?;
            for helper in &battery.helpers {
                contained.resolve(helper, "battery.helpers", EntryKind::File)?;
            }
            let base_document = read_document(&policy)?;
            let base = check_policy(
                &policy,
                &package.name,
                &battery.namespaces,
                &battery.helpers,
                &base_document,
            )?;
            // The loader finds the manifest beside the included base, and helper
            // commands run from the including policy's directory.
            if battery.targets.is_some() && policy.parent() != Some(contained.root.as_path()) {
                return Err(PackageError::TargetedPolicyOutsidePackageRoot {
                    field: "battery.policy",
                });
            }
            if battery.targets.is_none() {
                check_lookup_routes(&policy, &base)?;
            }
            if let Some(targets) = &mut battery.targets {
                check_stands_alone(&policy, &base_document, &base.tools)?;
                for target in BatteryTarget::ALL {
                    let target_policy = targets.get_mut(target);
                    let field = target.policy_field();
                    let target_path = contained.resolve(&target_policy.policy, field, EntryKind::File)?;
                    // The loader includes each resolved file once, so a target policy that
                    // is the base would be refused as a duplicate include.
                    if target_path == policy {
                        return Err(PackageError::TargetPolicyIsBase { field });
                    }
                    if target_path.parent() != Some(contained.root.as_path()) {
                        return Err(PackageError::TargetedPolicyOutsidePackageRoot { field });
                    }
                    let policy_path = target_path;
                    let document = read_document(&policy_path)?;
                    let bindings = check_policy(
                        &policy_path,
                        &package.name,
                        &battery.namespaces,
                        &battery.helpers,
                        &document,
                    )?;
                    check_composes(&policy_path, &base, &bindings)?;
                    check_variants(&policy_path, &bindings.tools, &base.tools)?;
                    check_lookup_routes(&policy_path, &bindings)?;
                    target_policy.audiences = bindings.audiences;
                    target_policy.credentials = bindings.credentials;
                }
            }
            battery.audiences = base.audiences;
            battery.credentials = base.credentials;
            let runs_a_helper = |command: &[String]| {
                runs_a_declared_helper(
                    &command.iter().map(String::as_str).collect::<Vec<_>>(),
                    &battery.helpers,
                )
            };
            let target_commands: Vec<&[String]> = battery
                .targets
                .iter()
                .flat_map(|targets| {
                    targets
                        .iter()
                        .filter_map(|(_, target_policy)| target_policy.readiness.as_deref())
                })
                .filter(|command| !command.is_empty())
                .collect();
            let readiness_error = |policy: PathBuf| {
                PackageError::Readiness { policy, reason: "a check runs python3 on a declared helper and requires python3; a target policy's check needs the base's [battery.readiness]; executables are bare names; alternatives reference owned credentials".into() }
            };
            if battery.readiness.is_none() && !target_commands.is_empty() {
                return Err(readiness_error(policy));
            }
            if let Some(check) = &battery.readiness {
                let executable = |name: &str| {
                    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                };
                let runs_python = !check.command.is_empty() || !target_commands.is_empty();
                let valid = (check.command.is_empty() || runs_a_helper(&check.command))
                    && target_commands.iter().all(|command| runs_a_helper(command))
                    && (!runs_python || check.required_executables.iter().any(|name| name == "python3"))
                    && check.required_executables.iter().all(|name| executable(name))
                    && check.cli_alternatives.iter().all(|alt| {
                        executable(&alt.executable)
                            && battery
                                .credentials
                                .iter()
                                .chain(battery.targets.iter().flat_map(|targets| {
                                    targets.iter().flat_map(|(_, target_policy)| &target_policy.credentials)
                                }))
                                .any(|credential| credential == &alt.credential)
                            && !alt.login_hint.trim().is_empty()
                            && !alt.login_hint.contains(['\n', '\r'])
                    });
                if !valid {
                    return Err(readiness_error(policy));
                }
            }
        }
        Role::Plugin(plugin) => {
            contained.resolve(plugin.default_policy(), "plugin.default_policy", EntryKind::File)?;
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

    fn resolve(&self, declared: &RelativePath, field: &'static str, kind: EntryKind) -> Result<PathBuf, PackageError> {
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
            true => EntryKind::Directory,
            false => EntryKind::File,
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

// ---------------------------------------------------------------------------
// The battery's policy
// ---------------------------------------------------------------------------

/// The top-level tables an included fragment may carry, and the `[policy]`
/// fields it may declare. Both sets are the config loader's (`compose_include`):
/// a package that validates here must load there, so a fragment refused at
/// someone's deployment is refused at the package instead. Beside these, a
/// fragment may carry `policy.deployment.confined_results` naming its own tools
/// (`check_confinement`).
const INCLUDABLE_TABLES: [&str; 2] = ["policy", "externals"];
pub const INCLUDABLE_POLICY_FIELDS: [&str; 5] = ["version", "tool", "annotator", "authority", "sanitizer"];
/// Those of them that are arrays of named declarations.
const DECLARATION_ARRAYS: [&str; 4] = ["tool", "annotator", "authority", "sanitizer"];

fn read_document(policy: &Path) -> Result<Value, PackageError> {
    let text = std::fs::read_to_string(policy).map_err(|source| PackageError::PolicyRead {
        policy: policy.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| PackageError::PolicySyntax {
        policy: policy.to_path_buf(),
        source,
    })
}

/// A battery is a fragment a deployment includes, not a deployment: it neither
/// includes further files nor sets the root-only externals, it runs only its own
/// declared helpers, and it names only contracts in the namespaces it declares.
/// Returns what the policy binds that the manifest alone cannot say: the audience
/// source providers, which a marketplace gives one owner each, and the credential
/// variables its helpers read, which an install names to the person.
fn check_policy(
    policy: &Path,
    name: &PackageName,
    namespaces: &[Namespace],
    helpers: &[RelativePath],
    document: &Value,
) -> Result<PolicyBindings, PackageError> {
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
    let Some(version) = declared.get("version").and_then(Value::as_integer) else {
        return Err(PackageError::PolicyField {
            policy: policy.to_path_buf(),
            field: "version".to_owned(),
        });
    };
    for (field, value) in declared {
        if field == "deployment" {
            check_confinement(policy, declared, value)?;
            continue;
        }
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

    let credentials = match document.get("externals") {
        Some(externals) => check_externals(policy, externals, name, helpers)?,
        None => Vec::new(),
    };
    let audiences: Vec<String> = document
        .get("externals")
        .and_then(|externals| externals.get("audience"))
        .and_then(Value::as_table)
        .map(|bindings| bindings.keys().cloned().collect())
        .unwrap_or_default();
    let lookups = document
        .get("externals")
        .and_then(|externals| externals.get("audience"))
        .and_then(Value::as_table)
        .into_iter()
        .flatten()
        .filter_map(|(name, binding)| Some((name.clone(), binding.get("lookup")?.as_str()?.to_owned())))
        .collect();

    // A contract may carry an argument filter — `mcp/ns/send(channel:C1)` — so
    // the check is on the family and namespace it opens with, not on the whole
    // id: parsing the tool half is the registry's job, not this one's.
    let covered: Vec<String> = namespaces
        .iter()
        .flat_map(|namespace| ["mcp", "host", "agent"].map(|family| format!("{family}/{namespace}/")))
        .collect();
    check_named(policy, document)?;
    let tools = declared_contracts(policy, document)?;
    for contract in &tools {
        if !covered.iter().any(|prefix| contract.starts_with(prefix)) {
            return Err(PackageError::PolicyForeignContract {
                policy: policy.to_path_buf(),
                contract: contract.clone(),
                namespaces: namespaces
                    .iter()
                    .map(Namespace::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }
    Ok(PolicyBindings {
        version,
        audiences,
        lookups,
        credentials,
        externals: externals(document).map(|(external, _)| external).collect(),
        tools,
        declarations: named_declarations(document),
    })
}

/// Every `[externals.<section>.<name>]` entry, and whether it runs a helper `command`.
fn externals(document: &Value) -> impl Iterator<Item = (NamedExternal, bool)> + '_ {
    document
        .get("externals")
        .and_then(Value::as_table)
        .into_iter()
        .flatten()
        .filter_map(|(section, entries)| Some((section, entries.as_table()?)))
        .flat_map(|(section, entries)| {
            entries.iter().map(move |(name, binding)| {
                let external = NamedExternal {
                    section: section.clone(),
                    name: name.clone(),
                };
                (external, binding.get("command").is_some())
            })
        })
}

/// Every field that holds an audience list, as `(declaration kind, path within it)`.
const AUDIENCE_FIELDS: [(&str, &[&str]); 7] = [
    ("tool", &["delta", "audience"]),
    ("tool", &["requires", "audience", "contains"]),
    ("tool", &["requires", "audience", "within"]),
    ("annotator", &["audiences"]),
    ("authority", &["permits", "audience_missing"]),
    ("sanitizer", &["permits", "audience", "from"]),
    ("sanitizer", &["permits", "audience", "to"]),
];

/// The `@<provider>:<selector>` entries the fragment's audience lists name; a named
/// group (`@finance`) carries no provider.
fn source_mentions(document: &Value) -> impl Iterator<Item = &str> {
    AUDIENCE_FIELDS
        .iter()
        .flat_map(|(kind, path)| {
            declarations(document, kind).filter_map(|declaration| {
                path.iter()
                    .try_fold(declaration, |value, key| value.get(key))
                    .and_then(Value::as_array)
            })
        })
        .flatten()
        .filter_map(Value::as_str)
        .filter(|entry| entry.starts_with('@') && entry.contains(':'))
}

/// A targeted battery's base is loaded alone wherever the sandbox is off, so it
/// runs no helper, every tool and annotator it names resolves within it, and every
/// declaration the runtime opens only with a binding is bound within it.
fn check_stands_alone(policy: &Path, base: &Value, tools: &[String]) -> Result<(), PackageError> {
    if let Some((external, _)) = externals(base).find(|(_, runs)| *runs) {
        return Err(PackageError::BaseRunsHelper {
            policy: policy.to_path_buf(),
            external,
        });
    }
    if let Some(entry) = source_mentions(base).next() {
        return Err(PackageError::BaseMentionsSource {
            policy: policy.to_path_buf(),
            entry: entry.to_owned(),
        });
    }
    let bound: BTreeSet<NamedExternal> = externals(base).map(|(external, _)| external).collect();
    let declared = named_declarations(base);
    let undeclared = bound.iter().find(|external| {
        DeclarationKind::ALL
            .into_iter()
            .filter(|kind| kind.section() == external.section)
            .any(|kind| {
                !declared.contains(&NamedDeclaration {
                    kind,
                    name: external.name.clone(),
                })
            })
    });
    if let Some(external) = undeclared {
        return Err(PackageError::BaseBindingUndeclared {
            policy: policy.to_path_buf(),
            external: external.clone(),
        });
    }
    let unbound = DeclarationKind::ALL
        .into_iter()
        .flat_map(|kind| {
            declarations(base, kind.field()).filter_map(move |declaration| {
                let name = declaration.get("name").and_then(Value::as_str)?;
                kind.binds_at_open(name, declaration).then(|| NamedDeclaration {
                    kind,
                    name: name.to_owned(),
                })
            })
        })
        .find(|declaration| !bound.contains(&declaration.binding()));
    if let Some(declaration) = unbound {
        return Err(PackageError::BaseDeclarationUnbound {
            policy: policy.to_path_buf(),
            declaration,
        });
    }
    let declared: BTreeSet<&str> = declarations(base, "annotator")
        .filter_map(|annotator| annotator.get("name").and_then(Value::as_str))
        .collect();
    let uses = declarations(base, "tool").filter_map(|tool| tool.get("annotator").and_then(Value::as_str));
    if let Some(annotator) = uses.into_iter().find(|annotator| !declared.contains(annotator)) {
        return Err(PackageError::BaseUsesTargetAnnotator {
            policy: policy.to_path_buf(),
            annotator: annotator.to_owned(),
        });
    }
    check_variants(policy, tools, &[])
}

/// The engine picks a tool's first matching variant, the composed order running
/// `policy` before `base`. No variant follows its tool's bare variant, a bare variant
/// does not shadow `base`'s patterned ones, and every tool `policy` names has a bare
/// variant here or in `base`, so no call falls through to `*`.
fn check_variants(policy: &Path, tools: &[String], base: &[String]) -> Result<(), PackageError> {
    fn split(variant: &str) -> (&str, bool) {
        match variant.split_once('(') {
            Some((tool, _)) => (tool, false),
            None => (variant, true),
        }
    }
    let patterned: BTreeSet<&str> = base
        .iter()
        .map(|other| split(other))
        .filter_map(|(tool, is_bare)| (!is_bare).then_some(tool))
        .collect();
    let mut bare = BTreeSet::new();
    for variant in tools {
        let refused = match split(variant) {
            (tool, _) if bare.contains(tool) => PackageError::UnreachableVariant {
                policy: policy.to_path_buf(),
                tool: tool.to_owned(),
                variant: variant.clone(),
            },
            (tool, true) if patterned.contains(tool) => PackageError::TargetShadowsBase {
                policy: policy.to_path_buf(),
                tool: tool.to_owned(),
            },
            (tool, is_bare) => {
                bare.extend(is_bare.then_some(tool));
                continue;
            }
        };
        return Err(refused);
    }
    bare.extend(
        base.iter()
            .map(|other| split(other))
            .filter_map(|(tool, is_bare)| is_bare.then_some(tool)),
    );
    match tools
        .iter()
        .map(|variant| split(variant).0)
        .find(|tool| !bare.contains(tool))
    {
        Some(tool) => Err(PackageError::ToolWithoutBareVariant {
            policy: policy.to_path_buf(),
            tool: tool.to_owned(),
        }),
        None => Ok(()),
    }
}

/// What `check_policy` reads out of a battery policy for its manifest, and what
/// a target policy must not repeat of its base.
struct PolicyBindings {
    version: i64,
    audiences: Vec<String>,
    /// Each audience binding that routes its member lookups, to the entry it names.
    lookups: BTreeMap<String, String>,
    credentials: Vec<String>,
    externals: BTreeSet<NamedExternal>,
    /// The declared tool contracts, canonical and in declaration order.
    tools: Vec<String>,
    declarations: BTreeSet<NamedDeclaration>,
}

/// One `[externals.<section>.<name>]` entry, or one field of the `jev` profile:
/// the loader composes each into the root's table and refuses it twice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamedExternal {
    pub section: String,
    pub name: String,
}

impl fmt::Display for NamedExternal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "externals.{}.{}", self.section, self.name)
    }
}

/// The declaration kinds the engine registry admits once per name across every
/// included file. Tool rules are absent: they are ordered matchers and may repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeclarationKind {
    Annotator,
    Authority,
    Sanitizer,
}

impl DeclarationKind {
    const ALL: [Self; 3] = [Self::Annotator, Self::Authority, Self::Sanitizer];

    fn field(self) -> &'static str {
        match self {
            Self::Annotator => "annotator",
            Self::Authority => "authority",
            Self::Sanitizer => "sanitizer",
        }
    }

    fn section(self) -> &'static str {
        match self {
            Self::Annotator => "annotators",
            Self::Authority => "authorities",
            Self::Sanitizer => "sanitizers",
        }
    }

    /// Whether the runtime refuses to open a policy that declares this without an
    /// `[externals]` binding: an authority may stay unbound, an annotator naming a
    /// builtin is complete as written, and the engine applies its reserved sanitizer.
    fn binds_at_open(self, name: &str, declaration: &Value) -> bool {
        match self {
            Self::Authority => false,
            Self::Annotator => declaration.get("builtin").is_none(),
            Self::Sanitizer => name != RESERVED_SANITIZER,
        }
    }
}

/// The sanitizer the engine applies itself (`SanitizerName::ATTEST_SCHEMA`).
const RESERVED_SANITIZER: &str = "attest-schema";

/// One `[[policy.<kind>]]` declaration, by name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamedDeclaration {
    pub kind: DeclarationKind,
    pub name: String,
}

impl fmt::Display for NamedDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "policy.{} {}", self.kind.field(), self.name)
    }
}

impl NamedDeclaration {
    fn binding(&self) -> NamedExternal {
        NamedExternal {
            section: self.kind.section().to_owned(),
            name: self.name.clone(),
        }
    }
}

/// Every once-per-name declaration in the fragment; `check_named` has already
/// refused one without a name.
fn named_declarations(document: &Value) -> BTreeSet<NamedDeclaration> {
    DeclarationKind::ALL
        .into_iter()
        .flat_map(|kind| {
            declarations(document, kind.field())
                .filter_map(|declaration| declaration.get("name").and_then(Value::as_str))
                .map(move |name| NamedDeclaration {
                    kind,
                    name: name.to_owned(),
                })
        })
        .collect()
}

/// A deployment of a target includes the target policy beside its base, so the target policy
/// meets the base under the loader's composition rules: one policy version, no
/// external the base already binds, and no annotator, authority or sanitizer the
/// base already names.
fn check_composes(policy: &Path, base: &PolicyBindings, target_policy: &PolicyBindings) -> Result<(), PackageError> {
    if target_policy.version != base.version {
        return Err(PackageError::TargetPolicyVersion {
            policy: policy.to_path_buf(),
            base: base.version,
            found: target_policy.version,
        });
    }
    if let Some(external) = base.externals.intersection(&target_policy.externals).next() {
        return Err(PackageError::TargetPolicyDuplicateExternal {
            policy: policy.to_path_buf(),
            external: external.clone(),
        });
    }
    match base.declarations.intersection(&target_policy.declarations).next() {
        Some(declaration) => Err(PackageError::TargetPolicyDuplicateDeclaration {
            policy: policy.to_path_buf(),
            declaration: declaration.clone(),
        }),
        None => Ok(()),
    }
}

/// The loader follows a lookup route one hop: it refuses a route whose destination
/// routes its own lookups. A targeted base binds no command-bearing audience, so
/// every route and every destination it may chain to live in one policy; a
/// destination it does not bind is the root's, which the loader checks against the
/// composed config.
fn check_lookup_routes(policy: &Path, bindings: &PolicyBindings) -> Result<(), PackageError> {
    for (name, target) in &bindings.lookups {
        match (name == target, bindings.lookups.contains_key(target)) {
            (true, _) => {
                return Err(PackageError::AudienceLookupToItself {
                    policy: policy.to_path_buf(),
                    name: name.clone(),
                });
            }
            (false, true) => {
                return Err(PackageError::ChainedAudienceLookup {
                    policy: policy.to_path_buf(),
                    name: name.clone(),
                    target: target.clone(),
                });
            }
            (false, false) => {}
        }
    }
    Ok(())
}

/// Returns the credential variables the bindings name, each once, in name order.
fn check_externals(
    policy: &Path,
    externals: &Value,
    name: &PackageName,
    helpers: &[RelativePath],
) -> Result<Vec<String>, PackageError> {
    let root_setting = |key: &str| PackageError::PolicyRootSetting {
        policy: policy.to_path_buf(),
        key: key.to_owned(),
    };
    let Some(table) = externals.as_table() else {
        return Err(root_setting(""));
    };
    let mut credentials = std::collections::BTreeSet::new();
    for (kind, value) in table {
        if kind == JEV_PROFILE {
            credentials.insert(check_jev_profile(policy, value, name)?);
            continue;
        }
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
            credentials.extend(check_binding(policy, binding, &format!("{kind}.{id}"), name, helpers)?);
        }
    }
    Ok(credentials.into_iter().collect())
}

/// The one profile an included file may carry. The runtime sends its key only to
/// TypeSafe's API, so a battery that routes tools to the `jev` annotator ships the
/// profile beside them, naming a credential of its own.
const JEV_PROFILE: &str = "jev";

/// Returns the credential variable the profile names.
fn check_jev_profile(policy: &Path, profile: &Value, name: &PackageName) -> Result<String, PackageError> {
    let refuse = || PackageError::PolicyJevProfile {
        policy: policy.to_path_buf(),
    };
    let variable = match profile
        .as_table()
        .map(|table| table.iter().collect::<Vec<_>>())
        .as_deref()
    {
        Some([(key, variable)]) if key.as_str() == "token_env" => variable.as_str().ok_or_else(refuse)?,
        _ => return Err(refuse()),
    };
    let prefix = name.credential_prefix();
    match prefix.owns(variable) {
        true => Ok(variable.to_owned()),
        false => Err(PackageError::PolicyForeignCredential {
            policy: policy.to_path_buf(),
            external: JEV_PROFILE.to_owned(),
            variable: variable.to_owned(),
            prefix,
        }),
    }
}

/// The external kinds an included file may bind. A battery is an included
/// fragment, so this is exactly the set the config loader accepts from one.
pub const BINDABLE_KINDS: [&str; 5] = ["authorities", "sanitizers", "annotators", "audience", "context"];

/// The stock sanitizers a battery may bind by `builtin` name: pure rewrites the
/// runtime ships, which reach nothing outside the process. The loader's stock
/// names for its sanitizer section (`builtins::Stock::for_section`).
pub const BINDABLE_STOCK_SANITIZERS: [&str; 2] = ["redact-email", "redact-secrets"];

/// One binding of one external. A battery runs the programs it ships and
/// nothing else: the `command` shape naming a declared helper is the one
/// program it may bind. The `url` shape would reach the network from inside a
/// fragment the deployment merely included, and a `builtin` other than a stock
/// sanitizer would name a runtime module, or a model, the deployment did not
/// choose — those are the root's to bind.
/// Returns the credential variables the binding names: one, a list, or none.
fn check_binding(
    policy: &Path,
    binding: &Value,
    external: &str,
    name: &PackageName,
    helpers: &[RelativePath],
) -> Result<Vec<String>, PackageError> {
    let prefix = name.credential_prefix();
    let refuse = || PackageError::PolicyExternalCommand {
        policy: policy.to_path_buf(),
        external: external.to_owned(),
    };
    let Some(table) = binding.as_table() else {
        return Err(refuse());
    };
    // The loader takes exactly one transport per binding, and a credential
    // only beside a program that could receive it.
    let mut transports = 0;
    let mut stock = false;
    let mut credentials = Vec::new();
    for (key, value) in table {
        match key.as_str() {
            "command" if string_argv(value).is_some_and(|argv| runs_a_declared_helper(&argv, helpers)) => {
                transports += 1;
            }
            "builtin" if external.starts_with("sanitizers.") && names_a_stock_sanitizer(value) => {
                transports += 1;
                stock = true;
            }
            // The runtime injects these variables into the helper process, so
            // whatever a package names here it receives. A package names only
            // the credentials it owns: outside its own prefix it would read
            // another package's, and outside the provider namespace entirely it
            // would read the deployment's own environment and fail to load.
            "token_env" => {
                let named = value.as_array().map_or(std::slice::from_ref(value), Vec::as_slice);
                match named
                    .iter()
                    .find(|variable| !variable.as_str().is_some_and(|v| prefix.owns(v)))
                {
                    None if !named.is_empty() => {
                        credentials = named.iter().filter_map(Value::as_str).map(str::to_owned).collect();
                    }
                    foreign => {
                        return Err(PackageError::PolicyForeignCredential {
                            policy: policy.to_path_buf(),
                            external: external.to_owned(),
                            variable: foreign.and_then(Value::as_str).unwrap_or_default().to_owned(),
                            prefix,
                        });
                    }
                }
            }
            // An audience source declares the selector templates it serves
            // beside its binding; the loader parses the templates themselves.
            "selectors" if external.starts_with("audience.") && declares_selectors(value) => {}
            // An audience source may send its member lookups to another
            // audience entry; the loader resolves the name in the composed config.
            "lookup"
                if external.starts_with("audience.") && value.as_str().is_some_and(|target| !target.is_empty()) => {}
            _ => return Err(refuse()),
        }
    }
    match (transports, stock && !credentials.is_empty()) {
        (1, false) => Ok(credentials),
        _ => Err(refuse()),
    }
}

fn names_a_stock_sanitizer(builtin: &Value) -> bool {
    builtin
        .as_str()
        .is_some_and(|name| BINDABLE_STOCK_SANITIZERS.contains(&name))
}

/// A `selectors` array as the config loader reads it: a non-empty list of
/// tables, each a `template` string with an optional `feeds` of `self` or
/// `internal`, no template twice. A template is `/`-separated non-empty
/// segments, each a literal or a `<variable>`, none starting with `$`.
fn declares_selectors(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    let mut seen = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(table) = entry.as_table() else {
            return false;
        };
        if !table.keys().all(|key| key == "template" || key == "feeds") {
            return false;
        }
        let Some(template) = table.get("template").and_then(Value::as_str) else {
            return false;
        };
        let well_formed = |segment: &str| {
            let bracketed = segment.starts_with('<') || segment.ends_with('>');
            !segment.is_empty()
                && !segment.starts_with('$')
                && (!bracketed || (segment.starts_with('<') && segment.ends_with('>') && segment.len() > 2))
        };
        if template.is_empty() || !template.split('/').all(well_formed) || seen.contains(&template) {
            return false;
        }
        seen.push(template);
        match table.get("feeds") {
            None => {}
            Some(feeds) if matches!(feeds.as_str(), Some("self" | "internal")) => {}
            Some(_) => return false,
        }
    }
    !seen.is_empty()
}

/// A battery ships the programs it runs, so an argv is exactly `python3` and one
/// of the helpers this manifest declares.
fn runs_a_declared_helper(argv: &[&str], helpers: &[RelativePath]) -> bool {
    match argv {
        ["python3", helper] => helpers.iter().any(|declared| declared.as_str() == *helper),
        _ => false,
    }
}

fn string_argv(command: &Value) -> Option<Vec<&str>> {
    command.as_array()?.iter().map(Value::as_str).collect()
}

/// The one deployment setting a fragment carries: `confined_results` naming
/// tools the fragment itself declares, so an output sanitizer it ships has a
/// result to run on. The loader (`compose_included_confinement`) unions the
/// names into the root's list and refuses every other `policy.deployment` key.
fn check_confinement(policy: &Path, declared: &toml::Table, deployment: &Value) -> Result<(), PackageError> {
    let refused = |field: &str| PackageError::PolicyField {
        policy: policy.to_path_buf(),
        field: format!("deployment.{field}"),
    };
    let deployment = deployment.as_table().ok_or_else(|| refused(""))?;
    if let Some(field) = deployment.keys().find(|field| field.as_str() != "confined_results") {
        return Err(refused(field));
    }
    let confined = deployment
        .get("confined_results")
        .and_then(Value::as_array)
        .ok_or_else(|| refused("confined_results"))?;
    let tools: Vec<&str> = declared
        .get("tool")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(|name| name.split('(').next().unwrap_or(name))
        .collect();
    for entry in confined {
        let tool = entry.as_str().ok_or_else(|| refused("confined_results"))?;
        if !tools.contains(&tool) {
            return Err(PackageError::PolicyConfinesForeignTool {
                policy: policy.to_path_buf(),
                tool: tool.to_owned(),
            });
        }
    }
    Ok(())
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
fn declared_contracts(policy: &Path, document: &Value) -> Result<Vec<String>, PackageError> {
    declarations(document, "tool")
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(|name| (tool, name)))
        .map(|(tool, name)| {
            let Some(server) = tool.get("server") else {
                return Ok(name.to_owned());
            };
            let invalid = || PackageError::PolicyServer {
                policy: policy.to_path_buf(),
                contract: name.to_owned(),
            };
            let server = server.as_str().ok_or_else(invalid)?;
            let (bare, suffix) = name
                .find('(')
                .map_or((name, ""), |index| (&name[..index], &name[index..]));
            let canonical = appa_runtime_api::CanonicalTool::of("mcp", server, bare).map_err(|_| invalid())?;
            Ok(format!("{}{suffix}", canonical.as_str()))
        })
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
    use crate::package::Plugin;

    const BATTERY_MANIFEST: &str = "schema = 1\nname = \"github\"\ndescription = \"GitHub MCP server\"\n\n\
         [battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n";

    const BOTH_TARGETS: &str = "\n[battery.targets.single-user]\npolicy = \"single-user.toml\"\n\
         [battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n";

    const SINGLE_USER_POLICY: &str = "[policy]\nversion = 2\n\n\
         [externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n\
         selectors = [{ template = \"viewer\", feeds = \"self\" }]\n";

    /// A targeted battery's base: loaded alone wherever the sandbox is off, it runs no helper.
    const TARGETED_BASE: &str = "[policy]\nversion = 2\n\n[[policy.tool]]\nname = \"mcp/github/get_me\"\ndelta = {}\n";

    const BATTERY_POLICY: &str = "[policy]\nversion = 2\n\n\
         [[policy.tool]]\nname = \"mcp/github/get_me\"\ndelta = {}\n\n\
         [externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n\
         selectors = [{ template = \"viewer\", feeds = \"self\" }, { template = \"repo/<owner>/<repo>/collaborators\" }]\n";

    /// A battery package on disk, with the policy body the caller wants.
    fn battery(policy: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("appa-package.toml"), BATTERY_MANIFEST).unwrap();
        fs::write(directory.path().join("appa.toml"), policy).unwrap();
        fs::write(directory.path().join("audience-source.py"), "print('{}')\n").unwrap();
        directory
    }

    #[test]
    fn server_qualified_native_battery_rules_stay_inside_owned_namespaces() {
        let native = BATTERY_POLICY.replace(
            "name = \"mcp/github/get_me\"",
            "name = \"get_me(owner:me)\"\nserver = \"github\"",
        );
        assert!(validate_package(battery(&native).path()).is_ok());
        let foreign = native.replace("server = \"github\"", "server = \"other\"");
        assert!(matches!(
            validate_package(battery(&foreign).path()),
            Err(PackageError::PolicyForeignContract { .. })
        ));
        for rule in [
            "name = \"get_me\"\nserver = 3",
            "name = \"get_me\"\nserver = \"bad/server\"",
            "name = \"mcp/github/get_me\"\nserver = \"github\"",
            "name = \"*\"\nserver = \"github\"",
        ] {
            let policy = BATTERY_POLICY.replace("name = \"mcp/github/get_me\"", rule);
            assert!(matches!(
                validate_package(battery(&policy).path()),
                Err(PackageError::PolicyServer { .. })
            ));
        }
        let unscoped = BATTERY_POLICY.replace("mcp/github/get_me", "get_me");
        assert!(matches!(
            validate_package(battery(&unscoped).path()),
            Err(PackageError::PolicyForeignContract { .. })
        ));
    }

    fn claude_code_adapter() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appa-package.toml"),
            "schema = 1\nname = \"claude-code\"\ndescription = \"Claude Code plugin\"\n\n\
             [plugin]\nhost = \"claude-code\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n",
        )
        .unwrap();
        fs::write(directory.path().join("default.appa.toml"), "[policy]\nversion = 2\n").unwrap();
        directory
    }

    fn kagent_adapter() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appa-package.toml"),
            "schema = 1\nname = \"kagent\"\ndescription = \"kagent plugin\"\n\n\
             [plugin]\nhost = \"kagent\"\nprotocol = 1\ndefault_policy = \"default.appa.toml\"\n\
             images = { adk = \"ghcr.io/x/adk@sha256:aa\" }\n",
        )
        .unwrap();
        fs::write(directory.path().join("default.appa.toml"), "[policy]\nversion = 2\n").unwrap();
        directory
    }

    #[test]
    fn readiness_cannot_execute_an_undeclared_helper_or_read_foreign_credentials() {
        let directory = battery(BATTERY_POLICY);
        let path = directory.path().join("appa-package.toml");
        let valid = format!(
            "{BATTERY_MANIFEST}\n[battery.readiness]\ncommand = [\"python3\", \"audience-source.py\"]\nrequired_executables = [\"python3\"]\n"
        );
        fs::write(&path, &valid).unwrap();
        assert!(validate_package(directory.path()).is_ok());
        // Executables alone run nothing under python3, so they need not name it.
        fs::write(
            &path,
            format!("{BATTERY_MANIFEST}\n[battery.readiness]\nrequired_executables = [\"gh\"]\n"),
        )
        .unwrap();
        assert!(validate_package(directory.path()).is_ok());
        for invalid in [
            valid.replace(
                "required_executables = [\"python3\"]",
                "required_executables = [\"gh\"]",
            ),
            valid.replace(
                "command = [\"python3\", \"audience-source.py\"]",
                "command = [\"python3\", \"../escape.py\"]",
            ),
            valid.replace(
                "required_executables = [\"python3\"]",
                "required_executables = [\"/tmp/python3\"]",
            ),
            format!(
                "{valid}\n[[battery.readiness.cli_alternatives]]\nexecutable = \"gh\"\ncredential = \"APPA_PROVIDER_SLACK_TOKEN\"\nlogin_hint = \"gh auth login\"\n"
            ),
        ] {
            fs::write(&path, invalid).unwrap();
            assert!(matches!(
                validate_package(directory.path()),
                Err(PackageError::Readiness { .. })
            ));
        }
    }

    #[test]
    fn a_battery_package_validates() {
        let directory = battery(BATTERY_POLICY);

        let package = validate_package(directory.path()).unwrap();

        assert_eq!(package.name.as_str(), "github");
        assert_eq!(package.battery().unwrap().hosts, vec![Host::ClaudeCode]);
        // The providers the policy binds are read from it, for the marketplace's ownership check.
        assert_eq!(package.battery().unwrap().audiences, vec!["github"]);
        assert!(package.battery().unwrap().credentials.is_empty());
    }

    /// A battery ships the `jev` profile with a credential of its own and nothing else.
    #[test]
    fn a_battery_ships_the_jev_profile_with_its_own_credential() {
        let with = |profile: &str| format!("{BATTERY_POLICY}\n[externals.jev]\n{profile}\n");

        let package = validate_package(battery(&with("token_env = \"APPA_PROVIDER_GITHUB_JEV\"")).path()).unwrap();
        assert_eq!(package.battery().unwrap().credentials, vec!["APPA_PROVIDER_GITHUB_JEV"]);

        assert!(matches!(
            validate_package(battery(&with("token_env = \"APPA_PROVIDER_JEV_API_KEY\"")).path()),
            Err(PackageError::PolicyForeignCredential { external, .. }) if external == "jev"
        ));
        for profile in [
            "token_env = \"APPA_PROVIDER_GITHUB_JEV\"\nurl = \"https://elsewhere.example\"",
            "url = \"https://elsewhere.example\"",
            "token_env = 1",
        ] {
            assert!(
                matches!(
                    validate_package(battery(&with(profile)).path()),
                    Err(PackageError::PolicyJevProfile { .. })
                ),
                "{profile}"
            );
        }
    }

    /// The variables a battery's helpers read come from its bindings, one entry
    /// per variable however many bindings name it, so an install can tell the
    /// person what to set without the manifest restating the policy.
    #[test]
    fn a_battery_credentials_are_read_from_its_bindings() {
        let bound = BATTERY_POLICY.replace("selectors", "token_env = \"APPA_PROVIDER_GITHUB_TOKEN\"\nselectors")
            + "[externals.annotators.\"github.visibility\"]\ncommand = [\"python3\", \"audience-source.py\"]\n\
             token_env = \"APPA_PROVIDER_GITHUB_TOKEN\"\n";

        let package = validate_package(battery(&bound).path()).unwrap();

        assert_eq!(
            package.battery().unwrap().credentials,
            vec!["APPA_PROVIDER_GITHUB_TOKEN"]
        );
    }

    /// Each target policy passes the base's checks, and its audiences and credentials
    /// are read from its own policy, not merged into the base's.
    #[test]
    fn a_battery_target_policy_is_checked_and_its_bindings_read_on_their_own() {
        let directory = battery(TARGETED_BASE);
        fs::write(
            directory.path().join("appa-package.toml"),
            format!("{BATTERY_MANIFEST}{BOTH_TARGETS}"),
        )
        .unwrap();
        fs::write(directory.path().join("single-user.toml"), SINGLE_USER_POLICY).unwrap();
        let target_policy = "[policy]\nversion = 2\n\n\
             [externals.audience.github-org]\ncommand = [\"python3\", \"audience-source.py\"]\n\
             token_env = \"APPA_PROVIDER_GITHUB_ORG_TOKEN\"\nselectors = [{ template = \"org/<org>\" }]\n";
        fs::write(directory.path().join("multi-user.toml"), target_policy).unwrap();

        let package = validate_package(directory.path()).unwrap();
        let validated = package.battery().unwrap();
        assert!(validated.audiences.is_empty());
        assert!(validated.credentials.is_empty());
        let targets = validated.targets.as_ref().unwrap();
        assert_eq!(targets.get(BatteryTarget::SingleUser).audiences, vec!["github"]);
        let multi_user = targets.get(BatteryTarget::MultiUser);
        assert_eq!(multi_user.audiences, vec!["github-org"]);
        assert_eq!(multi_user.credentials, vec!["APPA_PROVIDER_GITHUB_ORG_TOKEN"]);

        fs::write(
            directory.path().join("multi-user.toml"),
            target_policy.replace("APPA_PROVIDER_GITHUB_ORG_TOKEN", "APPA_PROVIDER_SLACK_TOKEN"),
        )
        .unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::PolicyForeignCredential { .. })
        ));

        fs::remove_file(directory.path().join("multi-user.toml")).unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::MissingPath {
                field: "battery.targets.multi-user.policy",
                ..
            })
        ));
    }

    const MULTI_USER_POLICY: &str = "[policy]\nversion = 2\n\n\
         [externals.audience.github-org]\ncommand = [\"python3\", \"audience-source.py\"]\n\
         token_env = \"APPA_PROVIDER_GITHUB_ORG_TOKEN\"\nselectors = [{ template = \"org/<org>\" }]\n";

    /// A battery with a multi-user target policy, and `manifest_tail` appended to its manifest.
    fn targeted_battery(target_policy: &str, manifest_tail: &str) -> tempfile::TempDir {
        let directory = battery(TARGETED_BASE);
        fs::write(
            directory.path().join("appa-package.toml"),
            format!("{BATTERY_MANIFEST}{manifest_tail}{BOTH_TARGETS}"),
        )
        .unwrap();
        fs::write(directory.path().join("single-user.toml"), SINGLE_USER_POLICY).unwrap();
        fs::write(directory.path().join("multi-user.toml"), target_policy).unwrap();
        directory
    }

    /// A deployment of the target holds the target policy's credentials beside the
    /// base's, so a CLI alternative may stand in for either.
    #[test]
    fn readiness_alternatives_may_name_a_target_policy_credential() {
        let readiness = "\n[[battery.readiness.cli_alternatives]]\nexecutable = \"gh\"\n\
             credential = \"APPA_PROVIDER_GITHUB_ORG_TOKEN\"\nlogin_hint = \"gh auth login\"\n";

        validate_package(targeted_battery(MULTI_USER_POLICY, readiness).path()).unwrap();
    }

    /// A target policy's check runs python3 on a declared helper, as the base's does,
    /// under the base's declared requirements.
    #[test]
    fn a_target_policy_readiness_command_runs_a_declared_helper() {
        let base_readiness = "\n[battery.readiness]\nrequired_executables = [\"python3\"]\n";
        let directory = targeted_battery(MULTI_USER_POLICY, base_readiness);
        let path = directory.path().join("appa-package.toml");
        let with = |tail: &str, command: &str| format!("{BATTERY_MANIFEST}{tail}{BOTH_TARGETS}readiness = {command}\n");
        fs::write(&path, with(base_readiness, "[\"python3\", \"audience-source.py\"]")).unwrap();
        let package = validate_package(directory.path()).unwrap();
        let Role::Battery(battery) = package.role else {
            panic!("a battery")
        };
        assert_eq!(
            battery.readiness_command(BatteryTarget::MultiUser),
            ["python3", "audience-source.py"]
        );

        for invalid in [
            with(base_readiness, "[\"python3\", \"check-multi-user.py\"]"),
            with(base_readiness, "[\"sh\", \"audience-source.py\"]"),
            with(base_readiness, "[\"python3\"]"),
            with("", "[\"python3\", \"audience-source.py\"]"),
            with(
                "\n[battery.readiness]\nrequired_executables = [\"gh\"]\n",
                "[\"python3\", \"audience-source.py\"]",
            ),
        ] {
            fs::write(&path, &invalid).unwrap();
            assert!(
                matches!(validate_package(directory.path()), Err(PackageError::Readiness { .. })),
                "{invalid}"
            );
        }
    }

    /// An empty target policy check is presence-only, as an empty base one is: it runs
    /// nothing, so it needs neither python3 nor the base's check.
    #[test]
    fn an_empty_target_policy_readiness_command_runs_nothing() {
        let directory = targeted_battery(MULTI_USER_POLICY, "");
        let path = directory.path().join("appa-package.toml");
        let with = |tail: &str| format!("{BATTERY_MANIFEST}{tail}{BOTH_TARGETS}readiness = []\n");
        let base_check = "\n[battery.readiness]\ncommand = [\"python3\", \"audience-source.py\"]\n\
             required_executables = [\"python3\"]\n";
        for tail in [
            "",
            "\n[battery.readiness]\nrequired_executables = [\"gh\"]\n",
            base_check,
        ] {
            fs::write(&path, with(tail)).unwrap();
            let package = validate_package(directory.path()).unwrap_or_else(|error| panic!("{tail}: {error}"));
            let Role::Battery(battery) = package.role else {
                panic!("a battery")
            };
            assert!(battery.readiness_command(BatteryTarget::MultiUser).is_empty(), "{tail}");
        }
        let package = validate_package(directory.path()).unwrap();
        assert_eq!(
            package.battery().unwrap().readiness_command(BatteryTarget::SingleUser),
            ["python3", "audience-source.py"]
        );
    }

    /// The loader refuses an external two included files both declare, and an
    /// included file at another policy version, so a target policy that cannot sit
    /// beside its base is refused here.
    #[test]
    fn a_target_policy_that_cannot_compose_with_its_base_is_refused() {
        let stock = "[externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\n";
        let directory = targeted_battery(&format!("{MULTI_USER_POLICY}\n{stock}"), "");
        fs::write(
            directory.path().join("appa.toml"),
            format!("{TARGETED_BASE}\n[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n\n{stock}"),
        )
        .unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::TargetPolicyDuplicateExternal { external, .. })
                if external == NamedExternal { section: "sanitizers".into(), name: "mask".into() }
        ));

        let declaring = |declaration: &str| format!("{MULTI_USER_POLICY}\n{declaration}");
        for (kind, name) in [
            (DeclarationKind::Annotator, "classifier"),
            (DeclarationKind::Authority, "review"),
        ] {
            let builtin = if kind == DeclarationKind::Annotator {
                "builtin = \"llm\"\n"
            } else {
                ""
            };
            let shared = format!("[[policy.{}]]\nname = \"{name}\"\n{builtin}", kind.field());
            let directory = targeted_battery(&declaring(&shared), "");
            fs::write(directory.path().join("appa.toml"), format!("{TARGETED_BASE}\n{shared}")).unwrap();
            let expected = NamedDeclaration {
                kind,
                name: name.to_owned(),
            };
            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::TargetPolicyDuplicateDeclaration { declaration, .. }) if declaration == expected
                ),
                "accepted a repeated {expected}"
            );
        }

        // Tool rules are ordered matchers, so a target policy may add one for a tool its base covers.
        let tool = "[[policy.tool]]\nname = \"mcp/github/get_me\"\ndelta = {}\n";
        validate_package(targeted_battery(&declaring(tool), "").path()).unwrap();

        let directory = targeted_battery(MULTI_USER_POLICY, "");
        fs::write(
            directory.path().join("appa-package.toml"),
            format!(
                "{BATTERY_MANIFEST}{}",
                BOTH_TARGETS.replace("multi-user.toml", "appa.toml")
            ),
        )
        .unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::TargetPolicyIsBase {
                field: "battery.targets.multi-user.policy"
            })
        ));

        let directory = targeted_battery(MULTI_USER_POLICY, "");
        fs::create_dir(directory.path().join("targets")).unwrap();
        fs::rename(
            directory.path().join("multi-user.toml"),
            directory.path().join("targets/multi-user.toml"),
        )
        .unwrap();
        fs::write(
            directory.path().join("appa-package.toml"),
            format!(
                "{BATTERY_MANIFEST}{}",
                BOTH_TARGETS.replace("\"multi-user.toml", "\"targets/multi-user.toml")
            ),
        )
        .unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::TargetedPolicyOutsidePackageRoot {
                field: "battery.targets.multi-user.policy"
            })
        ));

        let directory = targeted_battery(MULTI_USER_POLICY, "");
        fs::create_dir(directory.path().join("policies")).unwrap();
        for policy in ["appa.toml", "single-user.toml", "multi-user.toml"] {
            fs::rename(
                directory.path().join(policy),
                directory.path().join("policies").join(policy),
            )
            .unwrap();
        }
        fs::write(
            directory.path().join("appa-package.toml"),
            format!("{BATTERY_MANIFEST}{BOTH_TARGETS}").replace("policy = \"", "policy = \"policies/"),
        )
        .unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::TargetedPolicyOutsidePackageRoot {
                field: "battery.policy"
            })
        ));

        let versioned = MULTI_USER_POLICY.replace("version = 2", "version = 1");
        assert!(matches!(
            validate_package(targeted_battery(&versioned, "").path()),
            Err(PackageError::TargetPolicyVersion { base: 2, found: 1, .. })
        ));
    }

    #[test]
    fn an_adapter_package_of_each_host_validates() {
        let claude_code = claude_code_adapter();
        let kagent = kagent_adapter();

        assert!(matches!(
            validate_package(claude_code.path()).unwrap().plugin(),
            Some(Plugin::ClaudeCode { .. })
        ));
        assert!(matches!(
            validate_package(kagent.path()).unwrap().plugin(),
            Some(Plugin::Kagent { .. })
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
            "[\"python\", \"audience-source.py\"]",
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
            // Stock names bind only sanitizers: an authority that approves on
            // its own, or a model, is the root's choice.
            "[externals.authorities.review]\nbuiltin = \"approve\"\n",
            "[externals.sanitizers.mask]\nbuiltin = \"llm\"\n",
            // A command beside a url is still a url binding on the wire.
            "[externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n             url = \"https://elsewhere.example/audience\"\n",
            // A command is exactly python3 on one of the manifest's helpers.
            "[externals.audience.github]\ncommand = [\"python3\", \"check-multi-user.py\"]\n",
            "[externals.audience.github]\ncommand = [\"sh\", \"audience-source.py\"]\n",
            "[externals.audience.github]\ncommand = [\"python3\"]\n",
            "[externals.audience.github]\ncommand = [\"python3\", 3]\n",
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

    /// An audience source may route its member lookups to another audience
    /// entry by name; no other external, and no other value, carries one.
    #[test]
    fn an_audience_binding_may_route_its_lookups() {
        let binding = "[externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n";
        let routed = BATTERY_POLICY.replace(binding, &format!("{binding}lookup = \"readers\"\n"));
        assert!(validate_package(battery(&routed).path()).is_ok());

        for refused in [
            BATTERY_POLICY.replace(binding, &format!("{binding}lookup = \"\"\n")),
            BATTERY_POLICY.replace(binding, &format!("{binding}lookup = 3\n")),
            format!(
                "{BATTERY_POLICY}\n[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n\n\
                 [externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\nlookup = \"readers\"\n"
            ),
        ] {
            assert!(
                matches!(
                    validate_package(battery(&refused).path()),
                    Err(PackageError::PolicyExternalCommand { .. })
                ),
                "accepted {refused}"
            );
        }
    }

    /// A route the loader would refuse at every deployment is refused here: one to
    /// itself, and one to an entry the package binds that routes its own lookups.
    /// A destination the package does not bind is the root's to supply.
    #[test]
    fn a_lookup_route_takes_one_hop() {
        let binding = "[externals.audience.github]\ncommand = [\"python3\", \"audience-source.py\"]\n";
        let relay = "[externals.audience.relay]\ncommand = [\"python3\", \"audience-source.py\"]\n\
             selectors = [{ template = \"team/<team>\" }]\n";
        let routed = |to: &str| BATTERY_POLICY.replace(binding, &format!("{binding}lookup = \"{to}\"\n"));

        assert!(matches!(
            validate_package(battery(&routed("github")).path()),
            Err(PackageError::AudienceLookupToItself { name, .. }) if name == "github"
        ));
        assert!(matches!(
            validate_package(battery(&format!("{}{relay}lookup = \"host-members\"\n", routed("relay"))).path()),
            Err(PackageError::ChainedAudienceLookup { name, target, .. }) if name == "github" && target == "relay"
        ));
        assert!(validate_package(battery(&format!("{}{relay}", routed("relay"))).path()).is_ok());

        let target_policy = |lookup: &str| format!("{MULTI_USER_POLICY}lookup = \"{lookup}\"\n");
        let directory = targeted_battery(&target_policy("host-members"), "");
        assert!(validate_package(directory.path()).is_ok());
        fs::write(directory.path().join("multi-user.toml"), target_policy("github-org")).unwrap();
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::AudienceLookupToItself { name, .. }) if name == "github-org"
        ));
    }

    #[test]
    fn a_battery_binds_a_stock_sanitizer_by_name() {
        for stock in BINDABLE_STOCK_SANITIZERS {
            let directory = battery(&format!(
                "{BATTERY_POLICY}\n[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n\n\
                 [externals.sanitizers.mask]\nbuiltin = \"{stock}\"\n"
            ));
            assert!(
                validate_package(directory.path()).is_ok(),
                "refused the stock sanitizer {stock}"
            );
        }

        // The loader takes one transport per binding and hands a credential
        // only to a program; either beside a stock name would validate here
        // and refuse to load there.
        for extra in [
            "command = [\"python3\", \"audience-source.py\"]\n",
            "token_env = \"APPA_PROVIDER_GITHUB\"\n",
        ] {
            let directory = battery(&format!(
                "{BATTERY_POLICY}\n[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n\n\
                 [externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\n{extra}"
            ));
            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyExternalCommand { .. })
                ),
                "accepted a stock sanitizer beside {extra}"
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

    /// A helper that reads several credentials lists them; the battery's credentials are
    /// every binding's, and each listed variable is held to the package's own prefix.
    #[test]
    fn a_helper_may_list_several_credentials_each_its_own() {
        let listed = |vars: &str| {
            battery(&format!(
                "{}\n[externals.authorities.second]\ncommand = [\"python3\", \"audience-source.py\"]\n\
                 token_env = \"APPA_PROVIDER_GITHUB_B\"\n",
                BATTERY_POLICY.replace(
                    "command = [\"python3\", \"audience-source.py\"]\n",
                    &format!("command = [\"python3\", \"audience-source.py\"]\ntoken_env = {vars}\n"),
                )
            ))
        };

        let package =
            validate_package(listed("[\"APPA_PROVIDER_GITHUB_C\", \"APPA_PROVIDER_GITHUB_B\"]").path()).unwrap();
        assert_eq!(
            package.battery().unwrap().credentials,
            vec!["APPA_PROVIDER_GITHUB_B", "APPA_PROVIDER_GITHUB_C"]
        );

        for vars in [
            "[\"APPA_PROVIDER_GITHUB_C\", \"APPA_PROVIDER_SLACK_TOKEN\"]",
            "[\"APPA_PROVIDER_GITHUB_C\", 1]",
            "[]",
        ] {
            assert!(
                matches!(
                    validate_package(listed(vars).path()),
                    Err(PackageError::PolicyForeignCredential { .. })
                ),
                "accepted token_env = {vars}"
            );
        }
    }

    /// A `selectors` array is the one extra key an audience binding carries,
    /// and each entry is a `template` string with an optional `feeds` string.
    #[test]
    fn an_audience_binding_declares_selectors_in_the_loaders_shape() {
        let declared = "selectors = [{ template = \"viewer\", feeds = \"self\" }, { template = \"repo/<owner>/<repo>/collaborators\" }]\n";
        for replacement in [
            "selectors = [\"viewer\"]\n",
            "selectors = []\n",
            "selectors = [{ feeds = \"self\" }]\n",
            "selectors = [{ template = \"viewer\", feeds = 1 }]\n",
            "selectors = [{ template = \"viewer\", feeds = \"public\" }]\n",
            "selectors = [{ template = \"viewer\", roster = \"x\" }]\n",
            "selectors = [{ template = \"viewer\" }, { template = \"viewer\" }]\n",
            "selectors = [{ template = \"\" }]\n",
            "selectors = [{ template = \"repo//collaborators\" }]\n",
            "selectors = [{ template = \"channel/$id\" }]\n",
            "selectors = [{ template = \"channel/<id\" }]\n",
            "selectors = \"viewer\"\n",
        ] {
            let directory = battery(&BATTERY_POLICY.replace(declared, replacement));
            assert!(
                matches!(
                    validate_package(directory.path()),
                    Err(PackageError::PolicyExternalCommand { .. })
                ),
                "accepted {replacement}"
            );
        }
        let annotator = battery(&format!(
            "{BATTERY_POLICY}\n[externals.annotators.github]\ncommand = [\"python3\", \"audience-source.py\"]\n{declared}"
        ));
        assert!(
            matches!(
                validate_package(annotator.path()),
                Err(PackageError::PolicyExternalCommand { .. })
            ),
            "accepted selectors on an annotator binding"
        );
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
            (
                "[policy.deployment]\nstarting_label = { audience = [\"internal\"] }\n\n",
                "another root-only policy field",
            ),
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

    /// A battery confines the results of its own tools and nothing else: another
    /// tool's name, another deployment setting, or a shape that is not a list of
    /// names is refused before a deployment includes it.
    #[test]
    fn a_battery_confines_only_the_results_of_its_own_tools() {
        let head = "[policy]\nversion = 2\n\n";
        let own = "[policy.deployment]\nconfined_results = [\"mcp/github/get_me\"]\n\n";
        assert!(validate_package(battery(&BATTERY_POLICY.replace(head, &format!("{head}{own}"))).path()).is_ok());

        let foreign = "[policy.deployment]\nconfined_results = [\"mcp/github/list_repos\"]\n\n";
        assert!(matches!(
            validate_package(battery(&BATTERY_POLICY.replace(head, &format!("{head}{foreign}"))).path()),
            Err(PackageError::PolicyConfinesForeignTool { tool, .. }) if tool == "mcp/github/list_repos"
        ));

        for (body, what) in [
            (
                "[policy.deployment]\nconfined_results = [\"mcp/github/get_me\"]\nstarting_label = { audience = [\"internal\"] }\n\n",
                "a deployment setting beside the confinement",
            ),
            (
                "[policy.deployment]\nconfined_results = \"mcp/github/get_me\"\n\n",
                "a confinement that is not a list",
            ),
            (
                "[policy.deployment]\nconfined_results = [1]\n\n",
                "a confinement entry that is not a name",
            ),
        ] {
            assert!(
                matches!(
                    validate_package(battery(&BATTERY_POLICY.replace(head, &format!("{head}{body}"))).path()),
                    Err(PackageError::PolicyField { field, .. }) if field.starts_with("deployment")
                ),
                "accepted {what}"
            );
        }
    }

    /// A targeted battery with `base` as its base and `multi_user` as its multi-user target policy.
    fn targeted(base: &str, multi_user: &str) -> tempfile::TempDir {
        let directory = targeted_battery(multi_user, "");
        fs::write(directory.path().join("appa.toml"), base).unwrap();
        directory
    }

    /// Wherever the sandbox is off, a deployment loads a targeted battery's base alone,
    /// so the base runs no helper and names no audience source a helper would serve.
    #[test]
    fn a_targeted_base_runs_no_helper_and_names_no_audience_source() {
        let annotator = format!(
            "{TARGETED_BASE}\n[externals.annotators.visibility]\ncommand = [\"python3\", \"audience-source.py\"]\n"
        );
        for base in [BATTERY_POLICY.to_owned(), annotator] {
            assert!(matches!(
                validate_package(targeted(&base, MULTI_USER_POLICY).path()),
                Err(PackageError::BaseRunsHelper { .. })
            ));
        }

        let declaring = |declaration: &str| format!("{TARGETED_BASE}\n{declaration}");
        for declaration in [
            "[[policy.tool]]\nname = \"mcp/github/list\"\ndelta = { audience = [\"@github:viewer\"] }\n",
            "[[policy.annotator]]\nname = \"classifier\"\naudiences = [\"@github:viewer\"]\n",
        ] {
            assert!(matches!(
                validate_package(targeted(&declaring(declaration), MULTI_USER_POLICY).path()),
                Err(PackageError::BaseMentionsSource { entry, .. }) if entry == "@github:viewer"
            ));
        }
        let group = "[[policy.tool]]\nname = \"mcp/github/list\"\ndelta = { audience = [\"@finance\"] }\n";
        validate_package(targeted(&declaring(group), MULTI_USER_POLICY).path()).unwrap();
    }

    /// Loaded alone, a targeted base resolves its tools' annotators within itself, so
    /// one declared only by the target policies leaves the base unloadable.
    #[test]
    fn a_targeted_base_declares_the_annotators_its_tools_use() {
        let annotator = "[[policy.annotator]]\nname = \"classifier\"\nbuiltin = \"llm\"\n";
        let base =
            format!("{TARGETED_BASE}\n[[policy.tool]]\nname = \"mcp/github/list\"\nannotator = \"classifier\"\n");
        let with_targets = |base: &str, target_tail: &str| {
            let directory = targeted(base, &format!("{MULTI_USER_POLICY}\n{target_tail}"));
            fs::write(
                directory.path().join("single-user.toml"),
                format!("{SINGLE_USER_POLICY}\n{target_tail}"),
            )
            .unwrap();
            directory
        };

        assert!(matches!(
            validate_package(with_targets(&base, annotator).path()),
            Err(PackageError::BaseUsesTargetAnnotator { annotator, .. }) if annotator == "classifier"
        ));
        validate_package(with_targets(&format!("{base}\n{annotator}"), "").path()).unwrap();
    }

    /// Loaded alone, a targeted base has only its own bindings, so every declaration
    /// the runtime opens only with a binding is bound by the base itself.
    #[test]
    fn a_targeted_base_binds_every_declaration_it_opens_with() {
        let annotator = format!(
            "{TARGETED_BASE}\n[[policy.annotator]]\nname = \"classifier\"\n\n[[policy.tool]]\nname = \"mcp/github/list\"\nannotator = \"classifier\"\n"
        );
        let sanitizer = format!("{TARGETED_BASE}\n[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n");
        let stock_mask = "[externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\n";
        let with_targets = |base: &str, target_tail: &str| {
            let directory = targeted(base, &format!("{MULTI_USER_POLICY}\n{target_tail}"));
            fs::write(
                directory.path().join("single-user.toml"),
                format!("{SINGLE_USER_POLICY}\n{target_tail}"),
            )
            .unwrap();
            directory
        };

        for (base, target_binding, kind, name) in [
            (
                annotator.as_str(),
                "[externals.annotators.classifier]\ncommand = [\"python3\", \"classifier.py\"]\n",
                DeclarationKind::Annotator,
                "classifier",
            ),
            (sanitizer.as_str(), stock_mask, DeclarationKind::Sanitizer, "mask"),
        ] {
            let expected = NamedDeclaration {
                kind,
                name: name.to_owned(),
            };
            assert!(matches!(
                validate_package(with_targets(base, target_binding).path()),
                Err(PackageError::BaseDeclarationUnbound { declaration, .. }) if declaration == expected
            ));
        }

        let builtin_annotator =
            annotator.replace("name = \"classifier\"\n", "name = \"classifier\"\nbuiltin = \"llm\"\n");
        let base_bound_sanitizer = format!("{sanitizer}\n{stock_mask}");
        let reserved_sanitizer = sanitizer.replace("\"mask\"", &format!("\"{RESERVED_SANITIZER}\""));
        let unbound_authority =
            format!("{TARGETED_BASE}\n[[policy.authority]]\nname = \"operator\"\nhint = \"Ask the operator.\"\n");
        for base in [
            builtin_annotator,
            base_bound_sanitizer,
            reserved_sanitizer,
            unbound_authority,
        ] {
            validate_package(with_targets(&base, "").path()).unwrap();
        }
    }

    /// Loaded alone, a targeted base must declare each binding it carries, or the runtime
    /// refuses it as undeclared. A sanitizer's stock builtin is the one binding a base may
    /// carry without running a helper, so the check is exercised through it.
    #[test]
    fn a_targeted_base_declares_every_binding_it_carries() {
        let declaration = "[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n";
        let base = format!("{TARGETED_BASE}\n[externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\n");
        let directory = targeted(&base, &format!("{MULTI_USER_POLICY}\n{declaration}"));
        fs::write(
            directory.path().join("single-user.toml"),
            format!("{SINGLE_USER_POLICY}\n{declaration}"),
        )
        .unwrap();
        let expected = NamedExternal {
            section: "sanitizers".to_owned(),
            name: "mask".to_owned(),
        };
        assert!(matches!(
            validate_package(directory.path()),
            Err(PackageError::BaseBindingUndeclared { external, .. }) if external == expected
        ));
    }

    /// The engine picks a tool's first matching variant, target policy before base, and a
    /// call no variant matches falls through to `*`.
    #[test]
    fn every_tool_of_a_targeted_battery_has_a_reachable_bare_variant() {
        let tools = |names: &[&str]| -> String {
            names
                .iter()
                .map(|name| format!("[[policy.tool]]\nname = \"mcp/github/{name}\"\ndelta = {{}}\n"))
                .collect()
        };
        let validate = |base: &[&str], target: &[&str]| {
            validate_package(
                targeted(
                    &format!("[policy]\nversion = 2\n\n{}", tools(base)),
                    &format!("{MULTI_USER_POLICY}\n{}", tools(target)),
                )
                .path(),
            )
        };

        assert!(matches!(
            validate(&["list(owner:me)"], &[]),
            Err(PackageError::ToolWithoutBareVariant { tool, .. }) if tool == "mcp/github/list"
        ));
        validate(&["list(owner:me)", "list"], &[]).unwrap();
        for base in [["list", "list(owner:me)"], ["list", "list"]] {
            assert!(matches!(
                validate(&base, &[]),
                Err(PackageError::UnreachableVariant { tool, .. }) if tool == "mcp/github/list"
            ));
        }
        assert!(matches!(
            validate(&["list(owner:me)", "list"], &["list"]),
            Err(PackageError::TargetShadowsBase { tool, .. }) if tool == "mcp/github/list"
        ));
        validate(&["list"], &["list"]).unwrap();
        assert!(matches!(
            validate(&["list"], &["send(to:me)"]),
            Err(PackageError::ToolWithoutBareVariant { tool, .. }) if tool == "mcp/github/send"
        ));
        validate(&["send"], &["send(to:me)"]).unwrap();
    }

    /// A battery without targets is only ever composed whole, so it keeps its lookups
    /// and may leave a tool's other calls to `*`.
    #[test]
    fn an_untargeted_battery_keeps_its_lookups_and_patterned_tools() {
        let policy = format!(
            "{BATTERY_POLICY}\n[[policy.tool]]\nname = \"mcp/github/list(owner:me)\"\ndelta = {{ audience = [\"@github:viewer\"] }}\n"
        );
        validate_package(battery(&policy).path()).unwrap();
    }
}
