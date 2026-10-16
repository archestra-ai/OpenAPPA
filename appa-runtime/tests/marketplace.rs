//! The marketplace manifest against the marketplace it describes.
//!
//! `marketplace.toml` is what a deployment authenticates a package against, so
//! a digest that has drifted from the directory it names is the one failure
//! that must never reach a release. This runs in the repository, where the
//! packages are files; a deployment checks the same digests against what it
//! fetched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use appa_package::{Host, Marketplace, PackageKind, Role, TreeDigest, check_ownership, validate_package};
use appa_runtime::config::Config;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("appa-runtime is inside the repository")
        .to_path_buf()
}

fn marketplace_root() -> PathBuf {
    repository().join("marketplace")
}

fn manifest() -> Marketplace {
    Marketplace::read(&marketplace_root().join("marketplace.toml")).expect("the marketplace manifest reads")
}

#[test]
fn kagent_package_images_follow_the_release_registry_and_version() {
    let package = validate_package(&marketplace_root().join("plugins/kagent")).unwrap();
    let Role::Plugin(appa_package::Plugin::Kagent { images, .. }) = package.role else {
        panic!("the kagent package declares kagent images");
    };
    for (name, reference) in images {
        let expected = format!(
            "europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/appa-kagent-{}:v{}",
            name.as_str(),
            env!("CARGO_PKG_VERSION"),
        );
        assert_eq!(reference.as_str(), expected);
    }
}

/// Every package the manifest names is a package: it parses, its declared paths
/// exist, its policy names only namespaces it covers, and it runs only helpers
/// it ships.
///
/// And the packages hold together: a namespace and a credential each have one
/// owner. A deployment installs several of these side by side into one policy,
/// so a second package covering the first's namespace would append contracts to
/// tools the first never declared, and one naming a credential the first's
/// prefix covers would be handed it at spawn.
#[test]
fn every_package_the_marketplace_names_validates() {
    let root = marketplace_root();
    let mut packages = Vec::new();
    for entry in manifest().packages {
        let directory = root.join(entry.path.as_str());
        let package = validate_package(&directory).unwrap_or_else(|error| {
            panic!("{} does not validate: {error}", entry.path);
        });
        assert_eq!(
            package.name, entry.name,
            "{} is listed as {} and calls itself {}",
            entry.path, entry.name, package.name
        );
        // The kind is the table the package is listed under, and it decides how
        // a deployment installs it. A battery listed as an adapter parses,
        // validates and digests, and is installed as the wrong thing.
        let kind = match package.role {
            Role::Plugin(_) => PackageKind::Plugin,
            Role::Battery(_) => PackageKind::Battery,
        };
        assert_eq!(
            kind, entry.kind,
            "{} is listed as a {} and is a {kind}",
            entry.path, entry.kind
        );
        packages.push(package);
    }
    check_ownership(&packages).expect("the marketplace's packages hold together");
}

/// The recorded digest is the digest of the package's committed content, which
/// is what a deployment fetches and digests in turn. It is read out of Git
/// rather than off the working tree, because a working tree also holds what the
/// tests left behind — a `__pycache__` beside a battery's helper is not part of
/// the package, and never reaches the archive a deployment resolves. When this
/// fails, regenerate and commit the catalog with the package changes. Read the
/// catalog from the same archive: uncommitted catalog changes describe working
/// files, not HEAD. Check those with `bash scripts/appa-marketplace.sh --check`.
#[test]
fn every_recorded_digest_is_the_digest_of_its_committed_package() {
    let exported = tempfile::tempdir().expect("a temp dir is creatable");
    export_committed(exported.path());
    let root = exported.path().join("marketplace");
    let committed =
        Marketplace::read(&root.join("marketplace.toml")).expect("the committed marketplace manifest reads");

    let drifted: Vec<String> = committed
        .packages
        .iter()
        .filter_map(|entry| {
            let directory = root.join(entry.path.as_str());
            let found = TreeDigest::of_tree(&directory).unwrap_or_else(|error| {
                panic!("{} cannot be digested: {error}", entry.path);
            });
            (found != entry.digest).then(|| format!("[{}] {} -> {}", entry.path, entry.digest, found))
        })
        .collect();

    assert!(
        drifted.is_empty(),
        "HEAD's catalog does not match HEAD's packages.\n\
         Run `bash scripts/appa-marketplace.sh`, verify with `bash scripts/appa-marketplace.sh --check`, \
         and commit the catalog with the package changes before rerunning this test:\n  {}",
        drifted.join("\n  ")
    );
}

/// The marketplace subtree as `HEAD` holds it. `git archive` is how the build
/// script already reads committed content, and how a deployment will receive a
/// generation.
fn export_committed(destination: &Path) {
    let archive = Command::new("git")
        .arg("-C")
        .arg(repository())
        .args(["-c", "core.autocrlf=false"])
        .args(["archive", "--format=tar", "HEAD", "--", "marketplace"])
        .output()
        .expect("git runs");
    assert!(
        archive.status.success(),
        "git archive failed: {}",
        String::from_utf8_lossy(&archive.stderr)
    );
    let mut extract = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(destination)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("tar runs");
    extract
        .stdin
        .take()
        .expect("tar takes its input")
        .write_all(&archive.stdout)
        .expect("the archive is written");
    assert!(extract.wait().expect("tar finishes").success(), "tar failed");
}

/// A package directory that no entry names would ship unauthenticated, so the
/// manifest must list every one of them.
#[test]
fn every_package_directory_is_named_by_the_marketplace() {
    let root = marketplace_root();
    let listed: Vec<PathBuf> = manifest()
        .packages
        .iter()
        .map(|entry| root.join(entry.path.as_str()))
        .collect();

    let mut found = Vec::new();
    for kind in ["plugins", "batteries"] {
        let directory = root.join(kind);
        for entry in std::fs::read_dir(&directory).expect("the marketplace holds both package kinds") {
            let path = entry.expect("the directory entry reads").path();
            if path.join(appa_package::MANIFEST_FILE).is_file() {
                found.push(path);
            }
        }
    }

    let unlisted: Vec<&Path> = found
        .iter()
        .filter(|path| !listed.contains(path))
        .map(PathBuf::as_path)
        .collect();
    assert!(
        unlisted.is_empty(),
        "a package directory the marketplace does not name: {unlisted:?}"
    );
}

/// A battery that validates is a battery a deployment can include. The two
/// checks run in different crates and the dependency goes one way — the
/// validator is a leaf that cannot see the config loader, so it restates the
/// loader's rules rather than deriving them — so this composes each battery
/// into the root config of every host it declares and loads the result. A rule
/// one of them learns and the other does not shows up here as a battery that
/// validates and will not load.
#[test]
fn every_battery_composes_into_each_host_it_declares() {
    let root = marketplace_root();
    let manifest = manifest();
    let default_policy = |host: Host| match host {
        // AppenCode is source-distributed, without a marketplace plugin package.
        Host::OpenCode => repository().join("integrations/opencode/appa.toml"),
        _ => manifest
            .packages
            .iter()
            .find_map(
                |entry| match validate_package(&root.join(entry.path.as_str())).ok()?.role {
                    Role::Plugin(adapter) if adapter.host() == host => {
                        Some(root.join(entry.path.as_str()).join(adapter.default_policy().as_str()))
                    }
                    _ => None,
                },
            )
            .unwrap_or_else(|| panic!("the marketplace ships no adapter for {host}")),
    };

    for entry in &manifest.packages {
        let directory = root.join(entry.path.as_str());
        let Role::Battery(battery) = validate_package(&directory).expect("the package validates").role else {
            continue;
        };
        for host in &battery.hosts {
            // A deployment holds the package beside its root config and
            // includes it by a relative path, which is the only shape the
            // loader accepts. The composition is that arrangement.
            let composed = tempfile::tempdir().expect("a temp dir is creatable");
            let installed = composed.path().join(entry.name.as_str());
            copy_tree(&directory, &installed);
            let seed = std::fs::read_to_string(default_policy(*host)).expect("the adapter ships its default policy");
            let path = composed.path().join("appa.toml");
            std::fs::write(
                &path,
                format!("include = [\"{}/{}\"]\n{seed}", entry.name, battery.base),
            )
            .expect("the composed config is writable");

            Config::load(&path)
                .unwrap_or_else(|error| panic!("{} validates but does not compose into {host}: {error}", entry.path));
        }
    }
}

/// Where the test process's own PATH resolves `program`.
fn on_path(program: &str) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{program} is on the PATH"))
}

/// Every audience source a battery binds refuses a consult whose declared
/// templates are not the ones it serves, and does so before it reads a
/// credential: with no `APPA_PROVIDER_*` variable set and no CLI login on the
/// `PATH` to fall back to, the matching declaration reaches the token check
/// (exit 1) and a foreign one stops at the declaration check (exit 2). A
/// policy and a script of different versions never answer each other,
/// whatever the policy declares.
#[test]
fn every_bound_audience_source_checks_its_declaration_before_its_credential() {
    let root = marketplace_root();
    // The interpreter is run by its resolved path so the helper's PATH can be
    // empty: a `gh` on the developer's PATH would supply the credential.
    let no_path = tempfile::tempdir().expect("a temp dir is creatable");
    let mut checked = 0;
    for entry in &manifest().packages {
        let directory = root.join(entry.path.as_str());
        let Role::Battery(battery) = validate_package(&directory).expect("the package validates").role else {
            continue;
        };
        let installed = tempfile::tempdir().expect("a temp dir is creatable");
        copy_tree(&directory, installed.path());
        let targets = battery.targets.iter().flat_map(|targets| targets.iter());
        for file in std::iter::once(&battery.base).chain(targets.map(|(_, target)| &target.policy)) {
            let policy: toml::Value = toml::from_str(
                &std::fs::read_to_string(installed.path().join(file.as_str())).expect("the battery policy reads"),
            )
            .expect("the battery policy is TOML");
            for source in
                appa_runtime::config::source_registrations_of(&policy).expect("the battery declares its sources")
            {
                let provider = source.provider.as_str();
                let argv: Vec<&str> = policy["externals"]["audience"][provider]["command"]
                    .as_array()
                    .expect("a battery binds its source by a command")
                    .iter()
                    .map(|word| word.as_str().expect("an argv word"))
                    .collect();
                let served: Vec<String> = source
                    .templates
                    .iter()
                    .map(|t| t.template.as_str().to_owned())
                    .collect();
                let mut foreign = served.clone();
                foreign.push("foreign/<x>".to_owned());
                for (templates, expected) in [(served, 1), (foreign, 2)] {
                    let consult = serde_json::json!({
                        "version": 1,
                        "kind": "audience",
                        "name": provider,
                        "declaration": { "templates": templates },
                        "artifact": { "selector": "viewer" },
                    });
                    let mut child = Command::new(on_path(argv[0]))
                        .args(&argv[1..])
                        .current_dir(installed.path())
                        .env_clear()
                        .env("PATH", no_path.path())
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .expect("the helper spawns");
                    child
                        .stdin
                        .take()
                        .expect("stdin is piped")
                        .write_all(consult.to_string().as_bytes())
                        .expect("the consult writes");
                    let output = child.wait_with_output().expect("the helper exits");
                    assert_eq!(
                        output.status.code(),
                        Some(expected),
                        "{provider} of {}/{}: {}",
                        entry.path,
                        file.as_str(),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert!(
                        output.stdout.is_empty(),
                        "{provider} of {} answered without a credential",
                        entry.path
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked >= 3, "the shipped batteries bind audience sources");
}

/// One package directory, as a deployment would hold it.
fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("the destination is creatable");
    for entry in std::fs::read_dir(source).expect("the package directory reads") {
        let entry = entry.expect("the directory entry reads");
        let target = destination.join(entry.file_name());
        match entry.file_type().expect("the entry has a type").is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), target).expect("the file copies");
            }
        }
    }
}

/// A fragment that validates loads.
///
/// `appa-package` states what a distributed fragment may carry and
/// `appa-runtime`'s config loader states what any include may carry. The
/// dependency goes one way: `appa-package` is a leaf the build script loads,
/// so it restates the loader's rules and nothing derives one from the other.
/// Every rule one of them learns is a chance for them to disagree, and the
/// disagreement that matters has one direction: a package that validates and
/// then will not load breaks a deployment that trusted the marketplace.
///
/// The converse is deliberately false. A battery is a fragment someone else
/// wrote and a deployment installed, so the marketplace refuses what the loader
/// merely tolerates from a file the operator wrote themselves — a `url` binding
/// reaching an endpoint the deployment did not choose is the plain case. Those
/// are the `no, yes` rows below, and each one is a decision rather than a gap.
///
/// So each row carries both answers rather than the implication alone, which a
/// validator that refused everything would satisfy. The rows are crafted to sit
/// on each rule's edge; the shipped batteries sit in the middle of every rule
/// and would agree whatever the rules were.
#[test]
fn a_battery_that_validates_loads() {
    const HELPER: &str = "audience-source.py";
    let body = |declarations: &str, externals: &str| format!("[policy]\nversion = 2\n\n{declarations}\n{externals}");
    let tool = "[[policy.tool]]\nname = \"mcp/probe/read\"\ndelta = {}\n";
    let command = format!("[externals.audience.probe]\ncommand = [\"python3\", \"{HELPER}\"]\n");

    // Every row carries both answers. The marketplace's is the rule under
    // test; the loader's is what a deployment does with the same file, and
    // without it the rows the marketplace refuses would assert nothing.
    let yes = true;
    let no = false;
    let fragments = [
        ("a plain battery", yes, yes, body(tool, "")),
        (
            "a server-qualified native battery contract",
            yes,
            yes,
            body("[[policy.tool]]\nname = 'read(path:docs*)'\nserver = 'probe'\n", ""),
        ),
        (
            "a native battery contract outside its owned namespace",
            no,
            yes,
            body("[[policy.tool]]\nname = 'read'\nserver = 'other'\n", ""),
        ),
        ("a battery that runs its helper", yes, yes, body(tool, &command)),
        (
            "a provider credential this package owns",
            yes,
            yes,
            body(tool, &format!("{command}token_env = \"APPA_PROVIDER_PROBE\"\n")),
        ),
        // Stricter than the loader on purpose. A battery is a fragment someone
        // else wrote and a deployment installed; the loader is reading a file
        // the operator wrote themselves, and tolerates more from it.
        (
            "a credential another package owns",
            no,
            yes,
            body(tool, &format!("{command}token_env = \"APPA_PROVIDER_SLACK\"\n")),
        ),
        (
            "a url binding",
            no,
            yes,
            body(tool, "[externals.audience.probe]\nurl = \"http://127.0.0.1:9/x\"\n"),
        ),
        (
            "a declaration without a name",
            no,
            yes,
            body("[[policy.tool]]\ndelta = {}\n", ""),
        ),
        (
            "a stock sanitizer bound by name",
            yes,
            yes,
            body(
                &format!("{tool}\n[[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n"),
                "[externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\n",
            ),
        ),
        // Stricter than the loader again: the loader lets an operator's own
        // file bind the stock approving authority; a battery may not.
        (
            "a stock authority bound by name",
            no,
            yes,
            body(
                &format!(
                    "{tool}\n[[policy.authority]]\nname = \"nod\"\n[policy.authority.permits]\nattention = [\"hitl\"]\n"
                ),
                "[externals.authorities.nod]\nbuiltin = \"approve\"\n",
            ),
        ),
        // Refused by both, each for its own reason: neither crate reads the
        // other, so agreeing here is the thing worth holding.
        (
            "a builtin binding",
            no,
            no,
            body(tool, "[externals.audience.probe]\nbuiltin = \"llm\"\n"),
        ),
        (
            "a jev profile naming a credential this package owns",
            yes,
            yes,
            body(tool, "[externals.jev]\ntoken_env = \"APPA_PROVIDER_PROBE_JEV\"\n"),
        ),
        (
            "a jev profile naming a credential another package owns",
            no,
            yes,
            body(tool, "[externals.jev]\ntoken_env = \"APPA_PROVIDER_JEV_API_KEY\"\n"),
        ),
        (
            "a jev profile naming its endpoint",
            no,
            no,
            body(
                tool,
                "[externals.jev]\ntoken_env = \"APPA_PROVIDER_PROBE_JEV\"\nurl = \"https://elsewhere.example\"\n",
            ),
        ),
        (
            "a root-only external setting",
            no,
            no,
            body(tool, "[externals]\ntimeout_ms = 5000\n"),
        ),
        (
            "a credential outside the provider namespace",
            no,
            no,
            body(tool, &format!("{command}token_env = \"PROBE_TOKEN\"\n")),
        ),
        (
            "a root-only policy section",
            no,
            no,
            body(&format!("{tool}\n[policy.audience]\nteam = []\n"), ""),
        ),
        (
            "a battery confining the results of its own tool",
            yes,
            yes,
            body(
                &format!("{tool}\n[policy.deployment]\nconfined_results = [\"mcp/probe/read\"]\n"),
                "",
            ),
        ),
        (
            "a battery confining the results of a tool it does not declare",
            no,
            no,
            body(
                &format!("{tool}\n[policy.deployment]\nconfined_results = [\"mcp/probe/write\"]\n"),
                "",
            ),
        ),
        (
            "a deployment setting beside the confinement",
            no,
            no,
            body(
                &format!(
                    "{tool}\n[policy.deployment]\nconfined_results = [\"mcp/probe/read\"]\nstarting_label = {{ audience = [\"internal\"] }}\n"
                ),
                "",
            ),
        ),
        (
            "a top-level table only a root carries",
            no,
            no,
            format!("[deployment]\nname = \"x\"\n{}", body(tool, "")),
        ),
        ("no policy version", no, no, format!("[policy]\n\n{tool}")),
        (
            "a source declaring the selectors it serves",
            yes,
            yes,
            body(
                tool,
                &format!(
                    "{command}selectors = [{{ template = \"viewer\", feeds = \"self\" }}, {{ template = \"channel/<id>\" }}]\n"
                ),
            ),
        ),
        (
            "a selector feeding an audience outside the chain",
            no,
            no,
            body(
                tool,
                &format!("{command}selectors = [{{ template = \"viewer\", feeds = \"public\" }}]\n"),
            ),
        ),
        (
            "a selector declared twice",
            no,
            no,
            body(
                tool,
                &format!("{command}selectors = [{{ template = \"viewer\" }}, {{ template = \"viewer\" }}]\n"),
            ),
        ),
    ];

    for (what, validates_expected, loads_expected, fragment) in fragments {
        let package = tempfile::tempdir().expect("a temp dir is creatable");
        std::fs::write(
            package.path().join("appa-package.toml"),
            "schema = 1\nname = \"probe\"\ndescription = \"a fragment on a rule's edge\"\n\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n",
        )
        .expect("the manifest is writable");
        std::fs::write(package.path().join("appa.toml"), &fragment).expect("the policy is writable");
        std::fs::write(package.path().join(HELPER), "print('{}')\n").expect("the helper is writable");

        let validates = validate_package(package.path()).is_ok();
        let loads = loads_beside_a_root(package.path());

        assert_eq!(
            (validates, loads),
            (validates_expected, loads_expected),
            "{what}: the marketplace says {validates} and the loader says {loads}"
        );
        assert!(
            !validates || loads,
            "{what}: the package validates and then does not load"
        );
    }
}

/// Whether a deployment holding this package beside its root config loads.
fn loads_beside_a_root(package: &Path) -> bool {
    includes_load(package, &["appa.toml"])
}

/// A deployment holding this package beside its root config, and including
/// `policies` of it in that order; the root config's path.
fn deployment_including(package: &Path, policies: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let deployment = tempfile::tempdir().expect("a temp dir is creatable");
    let installed = deployment.path().join("probe");
    copy_tree(package, &installed);
    let root = deployment.path().join("appa.toml");
    let includes = policies
        .iter()
        .map(|policy| format!("\"probe/{policy}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        &root,
        format!(
            "include = [{includes}]\n[policy]\nversion = 2\n\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"
        ),
    )
    .expect("the root config is writable");
    (deployment, root)
}

/// Whether a deployment holding this package beside its root config, and
/// including `policies` of it in that order, loads.
fn includes_load(package: &Path, policies: &[&str]) -> bool {
    let (_deployment, root) = deployment_including(package, policies);
    Config::load(&root).is_ok()
}

/// An audience source the probe battery's helper serves.
fn probe_audience(name: &str) -> String {
    format!(
        "[externals.audience.{name}]\ncommand = [\"python3\", \"audience-source.py\"]\n\
         selectors = [{{ template = \"viewer\", feeds = \"self\" }}]\n"
    )
}

/// The external a targeted probe's base binds: its base runs no helper.
const PROBE_BASE_EXTERNAL: &str = "[externals.sanitizers.mask]\nbuiltin = \"redact-secrets\"\n";

/// A targeted battery on disk whose multi-user target policy binds `multi_user_external`.
/// The base and each target policy carry a rule of their own, so the composed rule
/// order shows which policies a deployment included and in what order.
fn targeted_probe(multi_user_external: &str) -> tempfile::TempDir {
    let package = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(
        package.path().join("appa-package.toml"),
        "schema = 1\nname = \"probe\"\ndescription = \"a targeted battery\"\n\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n\n[battery.targets.single-user]\npolicy = \"single-user.toml\"\n\n[battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n",
    )
    .expect("the manifest is writable");
    std::fs::write(
        package.path().join("appa.toml"),
        format!(
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/probe/read\"\ndelta = {{}}\n\n\
             [[policy.sanitizer]]\nname = \"mask\"\non = [\"tool_output\"]\n\n{PROBE_BASE_EXTERNAL}"
        ),
    )
    .expect("the base is writable");
    std::fs::write(
        package.path().join("single-user.toml"),
        format!(
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/probe/single_user\"\ndelta = {{}}\n\n{}",
            probe_audience("probe")
        ),
    )
    .expect("the single-user target policy is writable");
    std::fs::write(
        package.path().join("multi-user.toml"),
        format!(
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/probe/multi_user\"\ndelta = {{}}\n\n{multi_user_external}"
        ),
    )
    .expect("the multi-user target policy is writable");
    std::fs::write(package.path().join("audience-source.py"), "print('{}')\n").expect("the helper is writable");
    package
}

/// A deployment includes a targeted battery by its base, and each target's deployment
/// composes that target's policy before the base. A targeted battery that validates
/// loads so under every target, and one whose target policy redeclares an external of
/// its base is refused by both crates.
#[test]
fn a_targeted_battery_that_validates_loads_in_its_compose_order() {
    use appa_package::BatteryTarget;

    for (what, multi_user_external, composes) in [
        ("a distinct external", probe_audience("probe-org"), true),
        ("the base's external", PROBE_BASE_EXTERNAL.to_owned(), false),
    ] {
        let package = targeted_probe(&multi_user_external);
        let validated = validate_package(package.path());
        assert_eq!(validated.is_ok(), composes, "{what}: {validated:?}");

        let (_deployment, root) = deployment_including(package.path(), &["appa.toml"]);
        for (target, target_rule) in [
            (BatteryTarget::SingleUser, "mcp/probe/single_user"),
            (BatteryTarget::MultiUser, "mcp/probe/multi_user"),
        ] {
            let loaded = Config::load_for_target(&root, &[], target);
            match (composes, target) {
                (true, _) | (false, BatteryTarget::SingleUser) => assert_eq!(
                    loaded
                        .unwrap_or_else(|error| panic!("{what}: {target} loads: {error}"))
                        .tool_names(),
                    [target_rule, "mcp/probe/read"],
                    "{what}: {target}"
                ),
                (false, BatteryTarget::MultiUser) => {
                    assert!(loaded.is_err(), "{what}: the package is refused and yet loads")
                }
            }
        }
    }
}

/// The deployment target picks a battery's target policy, so a root config that
/// includes one directly is refused, whichever target it runs.
#[test]
fn a_targeted_battery_target_policy_is_not_included_directly() {
    use appa_package::BatteryTarget;
    use appa_runtime::config::ConfigError;

    let package = targeted_probe(&probe_audience("probe-org"));
    validate_package(package.path()).expect("the targeted package validates");
    let (_deployment, root) = deployment_including(package.path(), &["multi-user.toml"]);
    for target in BatteryTarget::ALL {
        assert!(
            matches!(
                Config::load_for_target(&root, &[], target),
                Err(ConfigError::TargetPolicyIncluded { path }) if path.ends_with("multi-user.toml")
            ),
            "{target}"
        );
    }
}

/// An include of a battery's base composes the deployment target's policy with it: each
/// target binds its own external, a battery without targets loads its base under
/// either, and a manifest that declares only some targets is refused.
#[test]
fn an_included_battery_loads_its_policy_for_the_deployment_target() {
    use appa_package::BatteryTarget;
    use appa_runtime::config::ConfigError;

    let deployment = |manifest_targets: &str, target_policies: &[(&str, &str)]| {
        let deployment = tempfile::tempdir().expect("a temp dir is creatable");
        let battery = deployment.path().join("probe");
        std::fs::create_dir(&battery).expect("the battery dir is creatable");
        std::fs::write(
            battery.join("appa-package.toml"),
            format!(
                "schema = 1\nname = \"probe\"\ndescription = \"a battery\"\n\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"audience-source.py\"]\n{manifest_targets}"
            ),
        )
        .expect("the manifest is writable");
        // A targeted base runs no helper, so it binds a stock sanitizer in place of a source.
        let base_external = match manifest_targets {
            "" => probe_audience("probe"),
            _ => "[[policy.sanitizer]]\nname = \"probe\"\non = [\"tool_output\"]\n\n\
                  [externals.sanitizers.probe]\nbuiltin = \"redact-secrets\"\n"
                .to_owned(),
        };
        std::fs::write(
            battery.join("appa.toml"),
            format!(
                "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/probe/read\"\ndelta = {{}}\n\n{base_external}"
            ),
        )
        .expect("the base is writable");
        for (file, external) in target_policies {
            std::fs::write(
                battery.join(file),
                format!("[policy]\nversion = 2\n\n{}", probe_audience(external)),
            )
            .expect("the target policy is writable");
        }
        std::fs::write(battery.join("audience-source.py"), "print('{}')\n").expect("the helper is writable");
        std::fs::write(
            deployment.path().join("appa.toml"),
            "include = [\"probe/appa.toml\"]\n[policy]\nversion = 2\n\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
        )
        .expect("the root config is writable");
        deployment
    };
    let load = |deployment: &tempfile::TempDir, target| {
        Config::load_for_target(&deployment.path().join("appa.toml"), &[], target)
    };
    let externals = |config: Config| {
        let mut bound: Vec<_> = (config.externals.audience.into_keys())
            .chain(config.externals.sanitizers.into_keys())
            .collect();
        bound.sort();
        bound
    };

    const BOTH: &str = "\n[battery.targets.single-user]\npolicy = \"single-user.toml\"\n\n[battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n";
    let targeted = deployment(
        BOTH,
        &[("single-user.toml", "probe-solo"), ("multi-user.toml", "probe-org")],
    );
    validate_package(&targeted.path().join("probe")).expect("the targeted package validates");
    assert_eq!(
        externals(load(&targeted, BatteryTarget::MultiUser).expect("multi-user loads")),
        ["probe", "probe-org"]
    );
    assert_eq!(
        externals(load(&targeted, BatteryTarget::SingleUser).expect("single-user loads")),
        ["probe", "probe-solo"]
    );

    let plain = deployment("", &[]);
    for target in BatteryTarget::ALL {
        assert_eq!(
            externals(load(&plain, target).expect("the base loads")),
            ["probe"],
            "{target}"
        );
    }

    let partial = deployment(
        "\n[battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n",
        &[("multi-user.toml", "probe-org")],
    );
    for target in BatteryTarget::ALL {
        assert!(
            matches!(
                load(&partial, target),
                Err(ConfigError::UnreadableBatteryManifest { source, .. })
                    if matches!(*source, appa_package::ManifestError::PartialTargets { missing: BatteryTarget::SingleUser, .. })
            ),
            "{target}"
        );
    }
}

/// Every rule the two crates share, not a sample of it.
///
/// `a_battery_that_validates_loads` walks fragments on each rule's edge, which
/// says nothing about a field or a binding kind no fragment happens to name.
/// These two lists are what `appa-package` states an included fragment may
/// carry, and each entry is a way for a package to validate here and be refused
/// by the loader there, so every entry gets its own fragment.
///
/// The loader keeps its own literals rather than reading these. It owns the
/// rule; the package crate restates it to refuse a package before a deployment
/// ever sees it, and reversing that would make a leaf crate normative for what
/// a deployment config may contain.
#[test]
fn every_field_and_binding_a_package_may_carry_loads() {
    for field in appa_package::INCLUDABLE_POLICY_FIELDS {
        if field == "version" {
            continue;
        }
        let fragment = format!("[policy]\nversion = 2\n\n[[policy.{field}]]\nname = \"probe\"\n");
        assert!(
            probe_loads(&fragment, false),
            "a package may declare `policy.{field}` and a deployment will not load it"
        );
    }

    for kind in appa_package::BINDABLE_KINDS {
        let fragment = format!(
            "[policy]\nversion = 2\n\n[externals.{kind}.probe]\ncommand = [\"python3\", \"audience-source.py\"]\n"
        );
        assert!(
            probe_loads(&fragment, true),
            "a package may bind `externals.{kind}` and a deployment will not load it"
        );
    }

    for stock in appa_package::BINDABLE_STOCK_SANITIZERS {
        let fragment = format!(
            "[policy]\nversion = 2\n\n[[policy.sanitizer]]\nname = \"probe\"\non = [\"tool_output\"]\n\n\
             [externals.sanitizers.probe]\nbuiltin = \"{stock}\"\n"
        );
        assert!(
            probe_loads(&fragment, false),
            "a package may bind the stock sanitizer {stock} and a deployment will not load it"
        );
    }
}

/// One fragment installed beside a root config, as `a_battery_that_validates_loads`
/// installs one, reporting only whether the deployment loads.
fn probe_loads(fragment: &str, with_helper: bool) -> bool {
    let package = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(package.path().join("appa.toml"), fragment).expect("the policy is writable");
    if with_helper {
        std::fs::write(package.path().join("audience-source.py"), "print('{}')\n").expect("the helper is writable");
    }
    loads_beside_a_root(package.path())
}
