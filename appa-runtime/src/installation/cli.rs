//! Marketplace command presentation. These commands never read stdin.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use appa_package::generation::{ArtifactDigest, Commit, Generation, Platform};
use appa_package::{Marketplace, PackageKind, PackageName, Role};
use clap::Args;
use serde::Serialize;

use super::{Acquired, InstallError, Installation, Requirements, Selection, includes};

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
    after_help = "Examples:\n  appa plugin list\n  appa battery list --config ./deployment/appa.toml --json\n\nLists every package in the catalog with its status. The catalog is the one\nretained for the installed version, read offline; before a first install it is\nthis build's own, which a release binary fetches."
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
    after_help = "Examples:\n  appa plugin install claude-code\n  appa plugin install kagent --config ./deployment/appa.toml --runtime both\n  appa plugin install claude-code --from ./appa-bundle.tar.gz --sha256 <trusted-sha256>\n\nClaude registration activates and verifies its runtime. Kagent prepares local\nHelm values, Agent snippets and image checks; it does not deploy to a cluster.\nNever prompts. An explicit revision moves the whole deployment to that version;\na bundle restores its selection."
)]
pub struct Install {
    /// Host plugin to install. Omitted, the catalog is listed instead.
    #[arg(value_parser = ["claude-code", "kagent"])]
    name: Option<String>,
    /// Kagent agent runtimes to prepare; both on first install, otherwise retained.
    #[arg(long, value_enum)]
    runtime: Option<super::kagent_images::KagentRuntime>,
    /// Let the agent report its own blocked calls to the OpenAPPA team through the
    /// `yell` tool. Written into the policy a first install creates; a later install
    /// keeps the config as it is. Without either flag, a terminal is asked.
    #[arg(long, conflicts_with = "no_agent_yell")]
    agent_yell: bool,
    /// Keep agent reporting off in the policy a first install creates, without asking.
    #[arg(long)]
    no_agent_yell: bool,
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
    after_help = "Examples:\n  appa plugin remove claude-code\n  appa plugin remove claude-code --config ./deployment/appa.toml --json\n  appa plugin remove claude-code --purge\n\nUnregisters only matching APPA-owned Claude support. Preserves policy, batteries,\nretained artifacts, trajectory data, and the running runtime. Never prompts.\n--purge goes on to stop the runtime and delete the data and config directories\nof the default deployment: the deployed binary, database, logs, retained\nversions, policy, and install state. appa itself stays on PATH."
)]
pub struct PluginRemove {
    /// Host plugin whose installer-owned registration is removed.
    #[arg(value_parser = ["claude-code", "kagent"])]
    name: String,
    /// Also stop the runtime and delete the default deployment's data and config directories.
    #[arg(long)]
    purge: bool,
    #[command(flatten)]
    target: Target,
}

pub fn remove_plugin(args: PluginRemove) -> ExitCode {
    if args.purge {
        return purge_plugin(args);
    }
    let result = (|| {
        let path = plugin_path(&args.target, &args.name)?;
        match Installation::inspect(&path) {
            Ok(None) => {
                return Ok((None, serde_json::json!({"plugin":args.name,"state":"unchanged"})));
            }
            Ok(Some(current)) if !current.plugins.contains(&args.name) => {
                return Ok((
                    Some(Version::of(current.generation())),
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
                Some(Version::of(selection.generation())),
                serde_json::json!({"plugin":args.name,"state":state}),
            ));
        }
        let before = super::required_bytes(installation.config_path())?;
        selection.deselect(
            PackageKind::Plugin,
            &PackageName::parse(&args.name).map_err(|error| InstallError::Invalid(error.to_string()))?,
        );
        eprintln!("appa: verifying ownership and removing {} support...", args.name);
        installation.commit_installation(Some(&before), &before, &selection)?;
        Ok((
            Some(Version::of(selection.generation())),
            serde_json::json!({"plugin":args.name,"state":"removed","runtime":"kept"}),
        ))
    })();
    finish(&args.target, "plugin.remove".into(), result)
}

/// `--purge` reads none of the installation state: a state the installer cannot
/// open is the state a purge exists for.
fn purge_plugin(args: PluginRemove) -> ExitCode {
    let result = (|| {
        if args.name != "claude-code" {
            return Err(InstallError::Invalid("--purge applies to claude-code".into()));
        }
        if args.target.config.is_some() {
            return Err(InstallError::Invalid(
                "--purge removes the default deployment only; drop --config and unset APPA_CONFIG".into(),
            ));
        }
        eprintln!("appa: removing claude-code support, stopping the runtime, and deleting the deployment...");
        let purge = crate::init::claude_code_purge().map_err(|error| InstallError::Invalid(error.to_string()))?;
        let runtime = match purge.runtime {
            crate::init::PurgedRuntime::Nothing => serde_json::json!({"state": "absent"}),
            crate::init::PurgedRuntime::Stopped { pid } => serde_json::json!({"state": "stopped", "pid": pid}),
            crate::init::PurgedRuntime::Left { reason } => serde_json::json!({"state": "left", "reason": reason}),
        };
        Ok((
            None,
            serde_json::json!({"plugin": "claude-code", "state": "purged", "runtime": runtime, "removed": purge.removed}),
        ))
    })();
    finish(&args.target, "plugin.purge".into(), result)
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
                "{} was set up without the marketplace, so no version is installed; run: appa plugin install claude-code",
                path.display()
            ))
        })?;
        let acquired = args
            .source
            .acquire(&installation, Some(&current), current.requirements())?;
        installation.retain(&acquired)?;
        let (_, battery) = super::battery_package(acquired.marketplace(), name.as_str())?;
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
        selection.set_generation(acquired.generation().clone());
        let mut text = includes::add(&text, &includes::battery_include(&name))?;
        if let Some(server) = &args.server {
            if battery.namespaces.len() != 1 {
                return Err(InstallError::Invalid("this battery has multiple namespaces; configure server_aliases explicitly in the deployment config".into()));
            }
            text = includes::bind_server(&text, &battery.namespaces[0], server)?;
        }
        eprintln!("appa: validating and activating the selected policy...");
        installation.commit_installation(Some(&before), text.as_bytes(), &selection)?;
        let prepared = prepared_directory(&installation)?;
        Ok((
            Some(Version::of(selection.generation())),
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
        let text = String::from_utf8(before.clone()).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let without = includes::remove(&text, &includes::battery_include(&args.name))?;
        if without == text && !selection.batteries.contains(args.name.as_str()) {
            return Ok((
                Some(Version::of(selection.generation())),
                serde_json::json!({"battery": args.name.as_str(), "state": "unchanged"}),
            ));
        }
        let acquired = Acquired::retained(&installation, &selection, Requirements::Packages)?;
        let (_, battery) = super::battery_package(acquired.marketplace(), args.name.as_str())?;
        let text = includes::unbind_servers(&without, &battery.namespaces)?;
        selection.deselect(PackageKind::Battery, &args.name);
        eprintln!("appa: validating and activating the remaining policy...");
        installation.commit_installation(Some(&before), text.as_bytes(), &selection)?;
        let prepared = prepared_directory(&installation)?;
        Ok((
            Some(Version::of(selection.generation())),
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
                                "the retained version names a different commit".into(),
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
                eprintln!("appa: verifying the retained version...");
                return Acquired::retained(installation, &selected, requirements);
            }
            eprintln!("appa: fetching missing artifacts for the installed version...");
            let acquired = match selected.generation().published() {
                Some(published) => Acquired::fetch(Some(published.release()), requirements)?,
                None if Acquired::is_own_build(selected.generation()) => Acquired::build(requirements)?,
                None => {
                    return Err(InstallError::Invalid(
                        "the retained build is incomplete and belongs to another build; reinstall with that build or with a published version (--revision)".into(),
                    ));
                }
            };
            if acquired.generation() != selected.generation() {
                return Err(InstallError::Invalid(
                    "the fetched version differs from the retained descriptor".into(),
                ));
            }
            return Ok(acquired);
        }
        if self.revision.is_some() || is_published_build() {
            eprintln!("appa: resolving the published version and fetching its artifacts...");
        }
        Acquired::own(self.revision.as_deref(), requirements)
    }
}

/// Whether the agent may report its own blocked calls through the `yell` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentYell {
    On,
    Off,
}

impl Install {
    /// The answer a first install writes into the policy it creates: the flag when
    /// one is given, the person's when a terminal is there to ask, and none
    /// otherwise, which leaves the shipped default. Never asked under `--json`.
    fn agent_yell(&self, config: &Path) -> Result<Option<AgentYell>, InstallError> {
        if self.agent_yell {
            return Ok(Some(AgentYell::On));
        }
        if self.no_agent_yell {
            return Ok(Some(AgentYell::Off));
        }
        let stdin = io::stdin();
        let stderr = io::stderr();
        if self.target.json || !stdin.is_terminal() || !stderr.is_terminal() {
            return Ok(None);
        }
        ask_agent_yell(&mut stdin.lock(), &mut stderr.lock())
            .map(Some)
            .map_err(|error| super::io("ask about agent reporting", config, error))
    }
}

/// One question, defaulting to yes on an empty line; end of input, where no one
/// is there to answer, is a no.
fn ask_agent_yell(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<AgentYell> {
    write!(
        output,
        "appa: send APPA's own decisions to the OpenAPPA team when it blocks a call?\n\
         Never your prompts, arguments, or outputs. Change later under `[reporting]`,\n\
         or send one yourself anytime with `appa yell`. [Y/n] "
    )?;
    output.flush()?;
    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        return Ok(AgentYell::Off);
    }
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => AgentYell::On,
        _ => AgentYell::Off,
    })
}

/// The line the shipped policy carries, and the line a yes replaces it with.
/// Anchored to the start of a line, so prose that quotes the setting is not
/// mistaken for it.
const AGENT_YELL_OFF: &str = "\nagent_yell = false";
const AGENT_YELL_ON: &str = "\nagent_yell = true";

fn with_agent_yell_on(policy: &str) -> String {
    policy.replacen(AGENT_YELL_OFF, AGENT_YELL_ON, 1)
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
        // Asked before the slow acquisition, so the person is not kept waiting to
        // answer, and before any state changes, so a closed terminal changes nothing.
        let agent_yell = if before.is_none() && name == "claude-code" {
            args.agent_yell(&path)?
        } else {
            None
        };
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
        // A bundle restores its own selection and a reinstall keeps the
        // person's battery choices; only the plugin's first install brings its
        // batteries along.
        let mut included = Vec::new();
        let (mut selection, text) = if let Some(imported) = acquired.imported() {
            if !imported.selection().plugins.contains(&name) {
                return Err(InstallError::Invalid(
                    "the bundle does not select the requested plugin".into(),
                ));
            }
            imported.configuration(&installation)?
        } else {
            let catalog = Marketplace::read(&acquired.marketplace().join("marketplace.toml"))
                .map_err(|error| InstallError::Invalid(error.to_string()))?;
            let entry = catalog
                .packages
                .iter()
                .find(|entry| entry.kind == PackageKind::Plugin && entry.name == package)
                .ok_or_else(|| InstallError::Invalid("plugin is absent from this version".into()))?;
            let root = acquired.marketplace().join(entry.path.as_str());
            let manifest = appa_package::Package::read(&root.join(appa_package::MANIFEST_FILE))
                .map_err(|error| InstallError::Invalid(error.to_string()))?;
            let Role::Plugin(plugin) = manifest.role else {
                return Err(InstallError::Invalid("selected package is not a plugin".into()));
            };
            let first_install = !current
                .as_ref()
                .is_some_and(|selection| selection.plugins.contains(&name));
            if first_install {
                included = super::host_batteries(acquired.marketplace(), plugin.host())?;
            }
            let selected = current.unwrap_or_else(|| Selection::empty(acquired.generation().clone(), platform));
            let text = match before.as_deref() {
                Some(bytes) => {
                    String::from_utf8(bytes.to_vec()).map_err(|error| InstallError::Invalid(error.to_string()))?
                }
                None => {
                    let text = String::from_utf8(super::required_bytes(&root.join(plugin.default_policy().as_str()))?)
                        .map_err(|error| InstallError::Invalid(error.to_string()))?;
                    match agent_yell {
                        Some(AgentYell::On) => with_agent_yell_on(&text),
                        Some(AgentYell::Off) | None => text,
                    }
                }
            };
            (selected, text)
        };
        selection.select(PackageKind::Plugin, &package);
        if let Some(runtime) = args.runtime {
            selection.kagent_runtime = Some(runtime);
        }
        selection.set_generation(acquired.generation().clone());
        let mut text = text;
        included.retain(|battery| !selection.batteries.contains(battery.as_str()));
        for battery in &included {
            selection.select(PackageKind::Battery, battery);
            text = includes::add(&text, &includes::battery_include(battery))?;
        }
        eprintln!("appa: verifying artifacts and preparing selected plugins...");
        installation.commit_installation(before.as_deref(), text.as_bytes(), &selection)?;
        let batteries: Vec<&str> = included.iter().map(PackageName::as_str).collect();
        // An existing config is the person's and is not edited; one without the
        // host's own battery gates nothing a Claude session does, so the gap is named.
        let warning = (name == "claude-code" && !includes::included(&text)?.contains("claude-code")).then(|| {
            format!(
                "{} does not include the claude-code battery: add \"{}\" to its include list, or {}",
                installation.config_path().display(),
                includes::battery_include(&PackageName::parse("claude-code").expect("a package name")),
                crate::init::START_OVER
            )
        });
        if let Some(warning) = &warning {
            eprintln!("appa: warning: {warning}");
        }
        let mut result = if name == "kagent" {
            let installed = installation
                .selection()?
                .ok_or_else(|| InstallError::Invalid("installation selection is missing".into()))?;
            let digest = installed
                .kagent_assets
                .ok_or_else(|| InstallError::Invalid("kagent preparation is missing".into()))?;
            serde_json::json!({"plugin":name,"state":"prepared","batteries":batteries,"directory":installation.state.join("kagent").join(digest.hex()),"cluster":"unchanged"})
        } else {
            serde_json::json!({"plugin": name, "state": "registered", "batteries": batteries, "runtime": "verified"})
        };
        if let Some(warning) = warning {
            result["warning"] = serde_json::Value::from(warning);
        }
        Ok((Some(Version::of(selection.generation())), result))
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

/// The version a deployment is at, as a person names it: the release tag when
/// one was published, otherwise the commit a build came from.
#[derive(Debug, Clone, Serialize)]
struct Version {
    commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    release: Option<String>,
}

impl Version {
    fn of(generation: &Generation) -> Self {
        Self {
            commit: generation.commit().to_string(),
            release: generation.published().map(|published| published.release().to_owned()),
        }
    }

    fn build(commit: &Commit) -> Self {
        Self {
            commit: commit.to_string(),
            release: None,
        }
    }

    fn label(&self) -> String {
        match &self.release {
            Some(release) => format!("version {release}"),
            None => format!("build {}", &self.commit[..self.commit.len().min(12)]),
        }
    }
}

#[derive(Serialize)]
struct Receipt {
    schema_version: u32,
    status: &'static str,
    operation: String,
    deployment: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<Version>,
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
fn listing(kind: PackageKind, target: &Target) -> Result<(Option<Version>, serde_json::Value), InstallError> {
    let path = target.path();
    let selection = Installation::inspect(&path)?;
    let installed = selection
        .as_ref()
        .map(|selection| selection.names(kind).clone())
        .unwrap_or_default();
    // A battery is included by the config's own include list, whoever wrote
    // the line, and stored when the store beside the config holds it.
    let included = match (kind, super::optional_bytes(&path)?) {
        (PackageKind::Battery, Some(bytes)) => {
            includes::included(std::str::from_utf8(&bytes).map_err(|error| InstallError::Invalid(error.to_string()))?)?
        }
        _ => Default::default(),
    };
    let store = crate::batteries::store_dir(&path);
    let deployment = match (&selection, path.exists()) {
        (Some(_), _) => "installed",
        (None, true) => "unmanaged",
        (None, false) => "absent",
    };
    let stage =
        tempfile::tempdir().map_err(|error| super::io("stage catalog", Path::new("temporary directory"), error))?;
    let mut fetched = None;
    let (source, version, marketplace) = match &selection {
        Some(selection) => (
            "retained",
            Some(Version::of(selection.generation())),
            Installation::retained_marketplace(&path, selection)?,
        ),
        None if is_published_build() => {
            let acquired = fetched.insert(Acquired::fetch(None, Requirements::Packages)?);
            (
                "published",
                Some(Version::of(acquired.generation())),
                acquired.marketplace().to_path_buf(),
            )
        }
        None => match Acquired::build_catalog(stage.path())? {
            (Some(commit), marketplace) => ("build", Some(Version::build(&commit)), marketplace),
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
            Ok(match kind {
                PackageKind::Plugin => serde_json::json!({
                    "name": entry.name.as_str(),
                    "installed": installed.contains(entry.name.as_str()),
                    "description": package.description,
                }),
                PackageKind::Battery => serde_json::json!({
                    "name": entry.name.as_str(),
                    "included": included.contains(entry.name.as_str()),
                    "stored": store.join(entry.name.as_str()).join("appa.toml").is_file(),
                    "description": package.description,
                }),
            })
        })
        .collect::<Result<Vec<_>, InstallError>>()?;
    let mut catalog = serde_json::json!({"source": source});
    if let Some(version) = &version {
        catalog["commit"] = serde_json::Value::from(version.commit.as_str());
    }
    Ok((
        version,
        serde_json::json!({"packages": packages, "deployment": deployment, "catalog": catalog}),
    ))
}

/// An install without a name is a request for orientation, not a mutation: the
/// catalog goes to stderr with the usage line, and the exit is the parser's.
fn orient(kind: PackageKind, target: &Target) -> ExitCode {
    let usage = format!("Usage: appa {kind} install <NAME>");
    if target.json {
        return usage_envelope(&format!(
            "a {kind} name is required. {usage}; list them with: appa {kind} list --json"
        ));
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
/// " with battery x" / " with batteries x and y" for a plugin receipt, empty when
/// the install included none.
fn with_batteries(result: &serde_json::Value) -> String {
    let names: Vec<&str> = result["batteries"]
        .as_array()
        .map(|batteries| batteries.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    match names.as_slice() {
        [] => String::new(),
        [one] => format!(" with battery {one}"),
        [head @ .., last] => format!(" with batteries {} and {last}", head.join(", ")),
    }
}

fn render_listing(output: &mut impl Write, kind: &str, result: &serde_json::Value, path: &Path) -> io::Result<()> {
    let packages = result["packages"].as_array().map(Vec::as_slice).unwrap_or_default();
    let name = |package: &serde_json::Value| package["name"].as_str().unwrap_or_default().to_owned();
    if packages.is_empty() {
        let source = result["catalog"]["source"].as_str().unwrap_or("selected");
        writeln!(output, "No {kind} packages in the {source} catalog.")?;
    } else {
        let width = packages
            .iter()
            .map(|package| name(package).chars().count())
            .max()
            .unwrap_or(0)
            .max(4);
        writeln!(output, "{:<width$}  {:<12}  DESCRIPTION", "NAME", "STATUS")?;
        for package in packages {
            let status = if package["installed"] == true {
                "● installed "
            } else if package["included"] == true {
                "● included  "
            } else if package["stored"] == false {
                "○ not stored"
            } else {
                "○ available "
            };
            writeln!(
                output,
                "{:<width$}  {status}  {}",
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
            .map(|selection| Version::of(selection.generation()));
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
    result: Result<(Option<Version>, serde_json::Value), InstallError>,
) -> ExitCode {
    let (receipt, code) = match result {
        Ok((version, result)) => (
            Receipt {
                schema_version: 1,
                status: "ok",
                operation,
                deployment: target.path(),
                version,
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
                InstallError::State { .. } => ("unreadable_state", false),
            };
            (
                Receipt {
                    schema_version: 1,
                    status: "error",
                    operation,
                    deployment: target.path(),
                    version: None,
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
        let mut stderr = io::stderr().lock();
        writeln!(stderr, "appa: {}", error.message).and_then(|()| {
            if error.recovery_required {
                writeln!(stderr, "appa: {}", crate::init::START_OVER)
            } else {
                Ok(())
            }
        })
    } else if let Some(kind) = receipt.operation.strip_suffix(".list") {
        let result = receipt
            .result
            .as_ref()
            .expect("a successful listing carries its result");
        render_listing(&mut output, kind, result, &receipt.deployment)
    } else if let Some(plugin) = receipt
        .result
        .as_ref()
        .and_then(|result| result.get("plugin"))
        .and_then(serde_json::Value::as_str)
    {
        if receipt.operation == "plugin.purge" {
            let result = receipt.result.as_ref().expect("plugin result is present");
            (|| {
                let runtime = &result["runtime"];
                match runtime["state"].as_str().unwrap_or_default() {
                    "stopped" => writeln!(output, "Stopped the runtime (pid {}).", runtime["pid"])?,
                    "left" => writeln!(
                        output,
                        "Left the process at the runtime endpoint: {}.",
                        runtime["reason"].as_str().unwrap_or_default()
                    )?,
                    _ => writeln!(output, "No runtime was running.")?,
                }
                for path in result["removed"].as_array().into_iter().flatten() {
                    writeln!(output, "Removed {}.", path.as_str().unwrap_or_default())?;
                }
                writeln!(
                    output,
                    "Plugin {plugin}: purged; appa stays on PATH. To install again: appa plugin install claude-code."
                )
            })()
        } else if receipt.operation == "plugin.remove" {
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
            let result = receipt.result.as_ref().expect("plugin result is present");
            writeln!(
                output,
                "Installed {plugin}{} for {} ({}); runtime verified.",
                with_batteries(result),
                receipt.deployment.display(),
                receipt
                    .version
                    .as_ref()
                    .map(Version::label)
                    .unwrap_or_else(|| "unknown version".to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer works by replacing the one line the shipped policy carries, so the
    /// policy has to carry exactly one of it. Two, or none, and a yes silently does nothing.
    #[test]
    fn the_shipped_policy_states_the_reporting_posture_exactly_once() {
        let text = crate::default_config::text();
        assert_eq!(text.matches(AGENT_YELL_OFF).count(), 1);
        assert_eq!(text.matches(AGENT_YELL_ON).count(), 0);
    }

    #[test]
    fn a_yes_turns_reporting_on_and_changes_only_that_line() {
        let before = crate::default_config::text();
        let after = with_agent_yell_on(&before);
        assert_ne!(after, before.as_ref());
        assert_eq!(after.replacen(AGENT_YELL_ON, AGENT_YELL_OFF, 1), before.as_ref());
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");
        std::fs::write(&config, &after).expect("the answered policy is written");
        crate::config::Config::load(&config).expect("the answered policy still loads");
    }

    #[test]
    fn an_empty_answer_is_yes_and_end_of_input_is_no() {
        let ask = |answer: &str| ask_agent_yell(&mut answer.as_bytes(), &mut Vec::new()).expect("the answer reads");
        for accepted in ["\n", "y\n", "yes\n", "Y\n", " yes \n"] {
            assert_eq!(ask(accepted), AgentYell::On, "{accepted:?}");
        }
        for declined in ["", "n\n", "no\n", "what?\n"] {
            assert_eq!(ask(declined), AgentYell::Off, "{declined:?}");
        }
    }
}
