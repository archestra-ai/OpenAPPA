//! Marketplace command presentation. These commands never read stdin.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use appa_package::generation::{ArtifactDigest, Platform};
use appa_package::{Marketplace, PackageKind, PackageName, Role};
use clap::Args;
use serde::Serialize;

use super::{Acquired, InstallError, Installation, Requirements, Selection};

#[derive(Debug, Args)]
pub struct Target {
    /// Deployment config; defaults to the installed local Claude config.
    #[arg(long, env = "APPA_CONFIG")]
    pub config: Option<PathBuf>,
    /// Emit one JSON result or error on stdout. Never prompts.
    #[arg(long)]
    pub json: bool,
}

impl Target {
    fn path(&self) -> PathBuf {
        self.config.clone().unwrap_or_else(crate::init::installed_config_path)
    }
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  appa plugin list\n  appa battery list --config ./deployment/appa.toml --json\n\nLists every package in the catalog with its status. The catalog is the one\nretained for the selected generation, read offline; before a first install it is\nthis build's own, which a release binary fetches."
)]
pub struct List {
    #[command(flatten)]
    target: Target,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Example:\n  appa bundle --config ./deployment/appa.toml --output ./appa-bundle.tar.gz --json\n\nList custom scripts and data in [bundle].files in your config; manual policy\nincludes are carried automatically. Interpreters, installed packages, credentials\nand container images are separate prerequisites. The archive includes your\nconfiguration: treat it as private."
)]
pub struct Bundle {
    #[command(flatten)]
    target: Target,
    /// New archive path. Refuses to overwrite an existing file.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  appa plugin install claude-code\n  appa plugin install kagent --config ./deployment/appa.toml --runtime both\n  appa plugin install claude-code --from ./appa-bundle.tar.gz --sha256 <trusted-sha256>\n\nClaude registration activates and verifies its runtime. Kagent prepares local\nHelm values, Agent snippets and image checks; it does not deploy to a cluster.\nNever prompts. An explicit revision updates the entire selected generation;\na bundle restores its selection."
)]
pub struct Install {
    /// Host plugin to install. Omitted, the catalog is listed instead.
    #[arg(value_parser = ["claude-code", "kagent"])]
    name: Option<String>,
    /// Kagent agent runtimes to prepare; both on first install, otherwise retained.
    #[arg(long, value_enum)]
    runtime: Option<super::kagent_images::KagentRuntime>,
    #[command(flatten)]
    target: Target,
    #[command(flatten)]
    source: Source,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  appa battery install github\n  appa battery install github --config ./deployment/appa.toml --server work-github\n\nAdds the policy include in this operation. Does not create an MCP connection,\nenable tools, acquire credentials, or activate optional authority bindings."
)]
pub struct BatteryInstall {
    /// Policy battery to add to this deployment. Omitted, the catalog is listed instead.
    #[arg(value_parser = package_name)]
    name: Option<PackageName>,
    #[command(flatten)]
    target: Target,
    #[command(flatten)]
    source: Source,
    /// Existing connection identity for a single-namespace battery.
    #[arg(long, value_parser = server_name)]
    server: Option<String>,
}

#[derive(Debug, Args)]
pub struct BatteryRemove {
    /// Selected battery whose unchanged installer-owned entries are removed.
    #[arg(value_parser = package_name)]
    name: PackageName,
    #[command(flatten)]
    target: Target,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  appa plugin remove claude-code\n  appa plugin remove claude-code --config ./deployment/appa.toml --json\n\nUnregisters only matching APPA-owned Claude support. Preserves policy, batteries,\nretained artifacts, trajectory data, and the running runtime. Never prompts."
)]
pub struct PluginRemove {
    /// Host plugin whose installer-owned registration is removed.
    #[arg(value_parser = ["claude-code", "kagent"])]
    name: String,
    #[command(flatten)]
    target: Target,
}

pub fn remove_plugin(args: PluginRemove) -> ExitCode {
    let result = (|| {
        let path = plugin_path(&args.target, &args.name)?;
        match Installation::inspect(&path) {
            Ok(None) => {
                return Ok((None, serde_json::json!({"plugin":args.name,"state":"unchanged"})));
            }
            Ok(Some(current)) if !current.plugins.contains(&args.name) => {
                return Ok((
                    Some(current.commit().to_string()),
                    serde_json::json!({"plugin":args.name,"state":"unchanged"}),
                ));
            }
            Ok(Some(_)) | Err(InstallError::Recovery { .. }) => {}
            Err(error) => return Err(error),
        }
        let installation = Installation::open(&path)?;
        let recovering = super::optional_bytes(&installation.state.join("transaction.json"))?.is_some();
        installation.recover_config()?;
        let state = if recovering { "recovered" } else { "unchanged" };
        let Some(mut selection) = installation.selection()? else {
            return Ok((None, serde_json::json!({"plugin":args.name,"state":state})));
        };
        if !selection.plugins.contains(&args.name) {
            return Ok((
                Some(selection.commit().to_string()),
                serde_json::json!({"plugin":args.name,"state":state}),
            ));
        }
        let before = super::required_bytes(installation.config_path())?;
        selection.deselect(
            PackageKind::Plugin,
            &PackageName::parse(&args.name).map_err(|error| InstallError::Invalid(error.to_string()))?,
        )?;
        eprintln!("appa: verifying ownership and removing {} support...", args.name);
        installation.commit_installation(Some(&before), &before, &selection)?;
        Ok((
            Some(selection.commit().to_string()),
            serde_json::json!({"plugin":args.name,"state":"removed","runtime":"kept"}),
        ))
    })();
    finish(&args.target, "plugin.remove".into(), result)
}

fn package_name(value: &str) -> Result<PackageName, String> {
    PackageName::parse(value).map_err(|error| error.to_string())
}
fn server_name(value: &str) -> Result<String, String> {
    appa_package::Namespace::parse(value).map_err(|error| error.to_string())?;
    Ok(value.to_owned())
}

pub fn install_battery(args: BatteryInstall) -> ExitCode {
    let Some(name) = args.name.clone() else {
        return orient(PackageKind::Battery, &args.target);
    };
    let result = (|| {
        let path = args.target.path();
        if !path.exists() {
            return Err(InstallError::Invalid(format!(
                "no deployment at {}; run: appa plugin install claude-code",
                path.display()
            )));
        }
        crate::config::Config::load(&path).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let installation = Installation::open(&path)?;
        installation.recover_config()?;
        let before = super::required_bytes(installation.config_path())?;
        let current = installation.selection()?.ok_or_else(|| {
            InstallError::Invalid(format!(
                "{} was set up without the marketplace, so no generation is selected; run: appa plugin install claude-code",
                path.display()
            ))
        })?;
        let acquired = args
            .source
            .acquire(&installation, Some(&current), current.requirements())?;
        installation.retain(&acquired)?;
        let catalog = Marketplace::read(&acquired.marketplace().join("marketplace.toml"))
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let entry = catalog
            .packages
            .iter()
            .find(|entry| entry.kind == PackageKind::Battery && entry.name == name)
            .ok_or_else(|| InstallError::Invalid(format!("battery {} is absent from this generation", name)))?;
        let package = appa_package::Package::read(
            &acquired
                .marketplace()
                .join(entry.path.as_str())
                .join(appa_package::MANIFEST_FILE),
        )
        .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let Role::Battery(battery) = package.role else {
            return Err(InstallError::Invalid("selected package is not a battery".into()));
        };
        let (mut selection, text) = match acquired.imported() {
            Some(imported) => {
                if !imported.selection().batteries.contains(name.as_str()) {
                    return Err(InstallError::Invalid(
                        "bundle does not select the requested battery".into(),
                    ));
                }
                imported.configuration(&installation)?
            }
            None => (
                current,
                String::from_utf8(before.clone()).map_err(|error| InstallError::Invalid(error.to_string()))?,
            ),
        };
        selection.select(PackageKind::Battery, &name);
        let mut text = selection.relocate(
            &text,
            acquired.generation().clone(),
            installation.config_path(),
            acquired.marketplace(),
        )?;
        let filename = installation
            .config_path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("installation requires UTF-8 config name");
        let include = format!(
            ".appa/{filename}/generations/{}/marketplace/{}/{}",
            selection.commit(),
            entry.path,
            battery.policy
        );
        text = selection.include_battery(&text, &name, &include)?;
        if let Some(server) = &args.server {
            if battery.namespaces.len() != 1 {
                return Err(InstallError::Invalid("this battery has multiple namespaces; configure server_aliases explicitly in the deployment config".into()));
            }
            text = selection.associate_battery(&text, &name, battery.namespaces[0].as_str(), server)?;
        }
        eprintln!("appa: validating and activating the selected policy...");
        installation.commit_installation(Some(&before), text.as_bytes(), &selection)?;
        let prepared = prepared_directory(&installation)?;
        Ok((
            Some(selection.commit().to_string()),
            battery_result(name.as_str(), "installed", prepared),
        ))
    })();
    finish(&args.target, "battery.install".into(), result)
}

pub fn remove_battery(args: BatteryRemove) -> ExitCode {
    let result = (|| {
        let path = args.target.path();
        crate::config::Config::load(&path).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let installation = Installation::open(&path)?;
        installation.recover_config()?;
        let before = super::required_bytes(installation.config_path())?;
        let mut selection = installation
            .selection()?
            .ok_or_else(|| InstallError::Invalid("no installed selection for this config".into()))?;
        if !selection.batteries.contains(args.name.as_str()) {
            return Ok((
                Some(selection.commit().to_string()),
                serde_json::json!({"battery": args.name.as_str(), "state": "unchanged"}),
            ));
        }
        let acquired = Acquired::retained(&installation, &selection, Requirements::Packages)?;
        let catalog = Marketplace::read(&acquired.marketplace().join("marketplace.toml"))
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let entry = catalog
            .packages
            .iter()
            .find(|entry| entry.kind == PackageKind::Battery && entry.name == args.name)
            .ok_or_else(|| InstallError::Invalid("selected battery is absent from its catalog".into()))?;
        let text = String::from_utf8(before.clone()).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let text = selection.remove_battery_include(&text, &args.name)?;
        let text = selection.remove_battery_aliases(&text, &args.name)?;
        selection.deselect(PackageKind::Battery, &args.name)?;
        installation.validate_removal(&text, &acquired.marketplace().join(entry.path.as_str()))?;
        eprintln!("appa: validating and activating the remaining policy...");
        installation.commit_installation(Some(&before), text.as_bytes(), &selection)?;
        let prepared = prepared_directory(&installation)?;
        Ok((
            Some(selection.commit().to_string()),
            battery_result(args.name.as_str(), "removed", prepared),
        ))
    })();
    finish(&args.target, "battery.remove".into(), result)
}

#[derive(Debug, Default, Args)]
struct Source {
    /// Published version tag or full commit. No automatic updates.
    #[arg(long, conflicts_with = "from", value_parser = revision)]
    revision: Option<String>,
    /// Restore a local deployment bundle with no marketplace network access.
    #[arg(long, requires = "sha256")]
    from: Option<PathBuf>,
    /// Full archive SHA256 obtained through a trusted channel.
    #[arg(long, requires = "from", value_parser = archive_digest)]
    sha256: Option<ArtifactDigest>,
}

impl Source {
    fn acquire(
        &self,
        installation: &Installation,
        current: Option<&Selection>,
        requirements: Requirements,
    ) -> Result<Acquired, InstallError> {
        if let (Some(bundle), Some(digest)) = (&self.from, &self.sha256) {
            eprintln!("appa: verifying the offline bundle...");
            return Acquired::import(bundle, digest);
        }
        let retained = if let Some(revision) = &self.revision {
            if let Ok(commit) = appa_package::generation::Commit::parse(revision) {
                let descriptor = installation
                    .state
                    .join("generations")
                    .join(commit.as_str())
                    .join(appa_package::generation::DESCRIPTOR_FILE);
                super::optional_bytes(&descriptor)?
                    .map(|bytes| {
                        let generation = appa_package::generation::Generation::parse(&bytes)
                            .map_err(|error| InstallError::Invalid(error.to_string()))?;
                        if generation.commit() != &commit {
                            return Err(InstallError::Invalid(
                                "retained generation names a different commit".into(),
                            ));
                        }
                        Ok(Selection::empty(
                            generation,
                            Platform::current().ok_or_else(|| InstallError::Invalid("unsupported platform".into()))?,
                        ))
                    })
                    .transpose()?
            } else {
                None
            }
        } else {
            current.cloned()
        };
        // A development build replaces a development generation with itself,
        // so a rebuilt checkout installs what it built. A published selection
        // stays until an explicit revision changes it.
        let retained = retained.filter(|selected| {
            self.revision.is_some()
                || is_published_build()
                || selected.generation().published().is_some()
                || Acquired::is_own_build(selected.generation())
        });
        if let Some(selected) = retained {
            let complete = super::acquisition::required_archives(selected.generation(), requirements)?
                .iter()
                .all(|name| {
                    installation
                        .state
                        .join("artifacts")
                        .join(selected.generation().archives()[name].hex())
                        .exists()
                });
            if complete {
                eprintln!("appa: verifying the retained generation...");
                return Acquired::retained(installation, &selected, requirements);
            }
            eprintln!("appa: fetching missing artifacts for the selected generation...");
            let acquired = match selected.generation().published() {
                Some(published) => Acquired::fetch(Some(published.release()), requirements)?,
                None if Acquired::is_own_build(selected.generation()) => Acquired::build(requirements)?,
                None => {
                    return Err(InstallError::Invalid(
                        "the retained development generation is incomplete and belongs to another build; reinstall with that build or a published generation".into(),
                    ));
                }
            };
            if acquired.generation() != selected.generation() {
                return Err(InstallError::Invalid(
                    "the acquired generation differs from the retained descriptor".into(),
                ));
            }
            return Ok(acquired);
        }
        if self.revision.is_some() || is_published_build() {
            eprintln!("appa: resolving the published generation and fetching its artifacts...");
        }
        Acquired::own(self.revision.as_deref(), requirements)
    }
}

fn revision(value: &str) -> Result<String, String> {
    super::acquisition::validate_revision(value).map_err(|error| error.to_string())?;
    Ok(value.to_owned())
}

fn archive_digest(value: &str) -> Result<ArtifactDigest, String> {
    ArtifactDigest::parse(&format!("sha256:{value}")).map_err(|error| error.to_string())
}

pub fn install(args: Install) -> ExitCode {
    let Some(name) = args.name.clone() else {
        return orient(PackageKind::Plugin, &args.target);
    };
    let result = (|| {
        if args.runtime.is_some() && name != "kagent" {
            return Err(InstallError::Invalid("--runtime applies only to kagent".into()));
        }
        let path = plugin_path(&args.target, &name)?;
        if path.exists() {
            crate::config::Config::load(&path).map_err(|error| InstallError::Invalid(error.to_string()))?;
        }
        let installation = Installation::open(&path)?;
        installation.recover_config()?;
        let before = super::optional_bytes(installation.config_path())?;
        let current = installation.selection()?;
        let platform = Platform::current()
            .ok_or_else(|| InstallError::Invalid("this platform has no published runtime binary".into()))?;
        let claude = name == "claude-code"
            || current
                .as_ref()
                .is_some_and(|selection| selection.plugins.contains("claude-code"));
        let kagent = name == "kagent"
            || current
                .as_ref()
                .is_some_and(|selection| selection.plugins.contains("kagent"));
        let requirements = match (claude, kagent) {
            (true, true) => Requirements::Both(platform),
            (true, false) => Requirements::Claude(platform),
            (false, true) => Requirements::Kagent,
            (false, false) => unreachable!("parser requires a supported plugin"),
        };
        let acquired = args.source.acquire(&installation, current.as_ref(), requirements)?;
        installation.retain(&acquired)?;
        let package = PackageName::parse(&name).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let (mut selection, text) = if let Some(imported) = acquired.imported() {
            if !imported.selection().plugins.contains(&name) {
                return Err(InstallError::Invalid(
                    "the bundle does not select the requested plugin".into(),
                ));
            }
            imported.configuration(&installation)?
        } else {
            let selected = current.unwrap_or_else(|| Selection::empty(acquired.generation().clone(), platform));
            let text = match before.as_deref() {
                Some(bytes) => {
                    String::from_utf8(bytes.to_vec()).map_err(|error| InstallError::Invalid(error.to_string()))?
                }
                None => {
                    let catalog = Marketplace::read(&acquired.marketplace().join("marketplace.toml"))
                        .map_err(|error| InstallError::Invalid(error.to_string()))?;
                    let entry = catalog
                        .packages
                        .iter()
                        .find(|entry| entry.kind == PackageKind::Plugin && entry.name == package)
                        .ok_or_else(|| InstallError::Invalid("plugin is absent from this generation".into()))?;
                    let root = acquired.marketplace().join(entry.path.as_str());
                    let package = appa_package::Package::read(&root.join(appa_package::MANIFEST_FILE))
                        .map_err(|error| InstallError::Invalid(error.to_string()))?;
                    let Role::Plugin(plugin) = package.role else {
                        return Err(InstallError::Invalid("selected package is not a plugin".into()));
                    };
                    String::from_utf8(super::required_bytes(&root.join(plugin.default_policy().as_str()))?)
                        .map_err(|error| InstallError::Invalid(error.to_string()))?
                }
            };
            (selected, text)
        };
        selection.select(PackageKind::Plugin, &package);
        if let Some(runtime) = args.runtime {
            selection.kagent_runtime = Some(runtime);
        }
        let text = selection.relocate(
            &text,
            acquired.generation().clone(),
            installation.config_path(),
            acquired.marketplace(),
        )?;
        eprintln!("appa: verifying artifacts and preparing selected plugins...");
        installation.commit_installation(before.as_deref(), text.as_bytes(), &selection)?;
        let result = if name == "kagent" {
            let installed = installation
                .selection()?
                .ok_or_else(|| InstallError::Invalid("installation selection is missing".into()))?;
            let digest = installed
                .kagent_assets
                .ok_or_else(|| InstallError::Invalid("kagent preparation is missing".into()))?;
            serde_json::json!({"plugin":name,"state":"prepared","directory":installation.state.join("kagent").join(digest.hex()),"cluster":"unchanged"})
        } else {
            serde_json::json!({"plugin": name, "state": "registered", "runtime": "verified"})
        };
        Ok((Some(selection.commit().to_string()), result))
    })();
    finish(&args.target, "plugin.install".into(), result)
}

/// A release build carries the tag whose generation the marketplace can fetch.
pub(crate) fn is_published_build() -> bool {
    option_env!("APPA_RELEASE_REF").is_some()
}

fn plugin_path(target: &Target, plugin: &str) -> Result<PathBuf, InstallError> {
    if plugin == "kagent" && target.config.is_none() {
        return Err(InstallError::Invalid(
            "kagent requires --config <deployment/appa.toml> (or APPA_CONFIG); no cluster is selected automatically"
                .into(),
        ));
    }
    Ok(target.path())
}

fn prepared_directory(installation: &Installation) -> Result<Option<PathBuf>, InstallError> {
    Ok(installation
        .selection()?
        .and_then(|selection| selection.kagent_assets)
        .map(|digest| installation.state.join("kagent").join(digest.hex())))
}

fn battery_result(name: &str, state: &str, prepared: Option<PathBuf>) -> serde_json::Value {
    let mut result = serde_json::json!({"battery":name,"state":state});
    if let Some(directory) = prepared {
        result["directory"] = serde_json::json!(directory);
        result["cluster"] = serde_json::json!("unchanged");
    }
    result
}

#[derive(Serialize)]
struct Receipt {
    schema_version: u32,
    status: &'static str,
    operation: String,
    deployment: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Failure>,
}

#[derive(Serialize)]
struct Failure {
    code: &'static str,
    message: String,
    recovery_required: bool,
}

pub fn list(kind: PackageKind, args: List) -> ExitCode {
    finish(&args.target, format!("{kind}.list"), listing(kind, &args.target))
}

/// Every package of `kind` in the catalog, with whether this deployment selects
/// it. The catalog is the retained one of the selected generation, read
/// offline; a deployment without a selection lists this build's own.
fn listing(kind: PackageKind, target: &Target) -> Result<(Option<String>, serde_json::Value), InstallError> {
    let path = target.path();
    let selection = Installation::inspect(&path)?;
    let installed = selection
        .as_ref()
        .map(|selection| selection.names(kind).clone())
        .unwrap_or_default();
    let deployment = match (&selection, path.exists()) {
        (Some(_), _) => "installed",
        (None, true) => "unmanaged",
        (None, false) => "absent",
    };
    let stage = tempfile::tempdir()
        .map_err(|error| super::io("stage catalog", Path::new("temporary directory"), error))?;
    let mut fetched = None;
    let (source, commit, marketplace) = match &selection {
        Some(selection) => (
            "retained",
            Some(selection.commit().clone()),
            Installation::retained_marketplace(&path, selection)?,
        ),
        None if is_published_build() => {
            let acquired = fetched.insert(Acquired::fetch(None, Requirements::Packages)?);
            (
                "published",
                Some(acquired.generation().commit().clone()),
                acquired.marketplace().to_path_buf(),
            )
        }
        None => match Acquired::build_catalog(stage.path())? {
            (Some(commit), marketplace) => ("build", Some(commit), marketplace),
            (None, marketplace) => ("checkout", None, marketplace),
        },
    };
    let catalog = Marketplace::read(&marketplace.join("marketplace.toml"))
        .map_err(|error| InstallError::Invalid(error.to_string()))?;
    let packages = catalog
        .packages
        .iter()
        .filter(|entry| entry.kind == kind)
        .map(|entry| {
            let manifest = marketplace.join(entry.path.as_str()).join(appa_package::MANIFEST_FILE);
            let package =
                appa_package::Package::read(&manifest).map_err(|error| InstallError::Invalid(error.to_string()))?;
            Ok(serde_json::json!({
                "name": entry.name.as_str(),
                "installed": installed.contains(entry.name.as_str()),
                "description": package.description,
            }))
        })
        .collect::<Result<Vec<_>, InstallError>>()?;
    let mut catalog = serde_json::json!({"source": source});
    if let Some(commit) = &commit {
        catalog["commit"] = serde_json::Value::from(commit.as_str());
    }
    Ok((
        commit.map(|commit| commit.to_string()),
        serde_json::json!({"packages": packages, "deployment": deployment, "catalog": catalog}),
    ))
}

/// An install without a name is a request for orientation, not a mutation: the
/// catalog goes to stderr with the usage line, and the exit is the parser's.
fn orient(kind: PackageKind, target: &Target) -> ExitCode {
    let usage = format!("Usage: appa {kind} install <NAME>");
    if target.json {
        return usage_envelope(&format!("a {kind} name is required. {usage}; list them with: appa {kind} list --json"));
    }
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let _ = match listing(kind, target) {
        Ok((_, result)) => render_listing(&mut stderr, kind.as_str(), &result, &target.path()),
        Err(error) => writeln!(stderr, "appa: {error}"),
    };
    let _ = writeln!(stderr, "\n{usage}");
    ExitCode::from(2)
}

/// The catalog as a table, then the deployment's state when nothing is installed
/// yet, so the next command is on the screen.
fn render_listing(output: &mut impl Write, kind: &str, result: &serde_json::Value, path: &Path) -> io::Result<()> {
    let packages = result["packages"].as_array().map(Vec::as_slice).unwrap_or_default();
    let name = |package: &serde_json::Value| package["name"].as_str().unwrap_or_default().to_owned();
    if packages.is_empty() {
        let source = result["catalog"]["source"].as_str().unwrap_or("selected");
        writeln!(output, "No {kind} packages in the {source} catalog.")?;
    } else {
        let width = packages.iter().map(|package| name(package).chars().count()).max().unwrap_or(0).max(4);
        writeln!(output, "{:<width$}  {:<9}  DESCRIPTION", "NAME", "STATUS")?;
        for package in packages {
            let status = if package["installed"] == true { "installed" } else { "available" };
            writeln!(
                output,
                "{:<width$}  {status:<9}  {}",
                name(package),
                package["description"].as_str().unwrap_or_default()
            )?;
        }
    }
    match result["deployment"].as_str() {
        Some("absent") => writeln!(
            output,
            "\nNo deployment at {} yet. Run: appa plugin install claude-code",
            path.display()
        ),
        Some("unmanaged") => writeln!(
            output,
            "\n{} was set up without the marketplace, so nothing is tracked here. Run: appa plugin install claude-code",
            path.display()
        ),
        _ => Ok(()),
    }
}

pub fn bundle(args: Bundle) -> ExitCode {
    let result = (|| {
        let installation = Installation::open(&args.target.path())?;
        let digest = installation.export_bundle(&args.output)?;
        let revision = installation
            .selection()?
            .map(|selection| selection.commit().to_string());
        Ok((
            revision,
            serde_json::json!({"archive": args.output, "sha256": digest.hex()}),
        ))
    })();
    finish(&args.target, "bundle".into(), result)
}

/// Parser failures happen before deployment resolution; they cannot truthfully
/// name a selected deployment or generation yet.
pub fn usage_error(error: &clap::Error) -> ExitCode {
    usage_envelope(&error.to_string())
}

fn usage_envelope(message: &str) -> ExitCode {
    let document = serde_json::json!({"schema_version": 1, "status": "error", "operation": "parse",
        "error": {"code": "usage", "message": message, "recovery_required": false}});
    let mut output = io::stdout().lock();
    match serde_json::to_writer(&mut output, &document)
        .map_err(io::Error::other)
        .and_then(|()| writeln!(output))
        .and_then(|()| output.flush())
    {
        Ok(()) => ExitCode::from(2),
        Err(_) => ExitCode::FAILURE,
    }
}

fn finish(
    target: &Target,
    operation: String,
    result: Result<(Option<String>, serde_json::Value), InstallError>,
) -> ExitCode {
    let (receipt, code) = match result {
        Ok((generation, result)) => (
            Receipt {
                schema_version: 1,
                status: "ok",
                operation,
                deployment: target.path(),
                generation,
                result: Some(result),
                error: None,
            },
            0,
        ),
        Err(error) => {
            let (code, recovery) = match &error {
                InstallError::Recovery { .. } => ("recovery_required", true),
                InstallError::Busy(_) => ("busy", false),
                InstallError::Changed(_) => ("concurrent_edit", false),
                InstallError::Invalid(_) => ("invalid_input", false),
                InstallError::Io { .. } => ("io", false),
            };
            (
                Receipt {
                    schema_version: 1,
                    status: "error",
                    operation,
                    deployment: target.path(),
                    generation: None,
                    result: None,
                    error: Some(Failure {
                        code,
                        message: error.to_string(),
                        recovery_required: recovery,
                    }),
                },
                if recovery { 3 } else { 1 },
            )
        }
    };
    let output = io::stdout();
    let mut output = output.lock();
    let written = if target.json {
        serde_json::to_writer(&mut output, &receipt)
            .map_err(io::Error::other)
            .and_then(|()| writeln!(output))
    } else if let Some(error) = &receipt.error {
        writeln!(io::stderr().lock(), "appa: {}", error.message)
    } else if let Some(kind) = receipt.operation.strip_suffix(".list") {
        let result = receipt.result.as_ref().expect("a successful listing carries its result");
        render_listing(&mut output, kind, result, &receipt.deployment)
    } else if let Some(plugin) = receipt
        .result
        .as_ref()
        .and_then(|result| result.get("plugin"))
        .and_then(serde_json::Value::as_str)
    {
        if receipt.operation == "plugin.remove" {
            let result = receipt.result.as_ref().expect("plugin result is present");
            writeln!(
                output,
                "Plugin {plugin}: {} for {}; configuration and data preserved{}.",
                result["state"].as_str().unwrap_or_default(),
                receipt.deployment.display(),
                if result["runtime"] == "kept" {
                    ", and the runtime keeps running"
                } else {
                    ""
                }
            )
        } else if plugin == "kagent" {
            let result = receipt.result.as_ref().expect("plugin result is present");
            writeln!(
                output,
                "Prepared kagent for {} at {}. No cluster changes. Read KAGENT.md and CONFIGURATION.txt there before deploying.",
                receipt.deployment.display(),
                result["directory"].as_str().unwrap_or_default()
            )
        } else {
            writeln!(
                output,
                "Installed {plugin} for {} (generation {}); runtime verified.",
                receipt.deployment.display(),
                receipt.generation.as_deref().unwrap_or("unknown")
            )
        }
    } else if let Some(battery) = receipt.result.as_ref().and_then(|result| result.get("battery")) {
        (|| {
            let result = receipt.result.as_ref().expect("battery results are present");
            writeln!(
                output,
                "Battery {}: {} for {}.",
                battery.as_str().unwrap_or_default(),
                result["state"].as_str().unwrap_or_default(),
                receipt.deployment.display()
            )?;
            if let Some(directory) = result.get("directory").and_then(serde_json::Value::as_str) {
                writeln!(
                    output,
                    "Updated kagent preparation: {directory}. Reapply explicitly; no cluster changes."
                )?;
            }
            Ok(())
        })()
    } else if let Some(archive) = receipt.result.as_ref().and_then(|result| result.get("archive")) {
        let result = receipt.result.as_ref().expect("bundle results are present");
        writeln!(
            output,
            "Bundle: {}\nSHA256: {}\nContains deployment configuration; keep it private.",
            archive.as_str().unwrap_or_default(),
            result["sha256"].as_str().unwrap_or_default()
        )
    } else {
        writeln!(
            output,
            "{}",
            receipt.result.as_ref().expect("successful receipts contain a result")
        )
    };
    if written.and_then(|()| output.flush()).is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::from(code)
    }
}
