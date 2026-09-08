//! Locally prepared host settings and operator-invoked image verification.

use std::collections::BTreeMap;

use appa_package::generation::{Generation, IMAGE_REGISTRY, Image};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::InstallError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(super) enum KagentRuntime {
    Python,
    Go,
    Both,
}

impl KagentRuntime {
    fn languages(self) -> &'static [(&'static str, Image)] {
        match self {
            Self::Python => &[("python", Image::Python)],
            Self::Go => &[("go", Image::Go)],
            Self::Both => &[("python", Image::Python), ("go", Image::Go)],
        }
    }
}

pub(super) fn artifacts(
    generation: &Generation,
    runtime: KagentRuntime,
) -> Result<BTreeMap<String, Vec<u8>>, InstallError> {
    let mut files = BTreeMap::new();
    let (registry, prefix) = IMAGE_REGISTRY
        .split_once('/')
        .ok_or_else(|| InstallError::Invalid("published image registry lacks its repository prefix".into()))?;
    let mut documents = BTreeMap::new();
    documents.insert(
        "kagent-values.json".to_owned(),
        json!({"controller": {"agentImage": {
            "registry": registry, "repository": format!("{prefix}/appa-kagent-adk"),
            "tag": generation.release(), "pullPolicy": "IfNotPresent"
        }}}),
    );
    let mut images = BTreeMap::new();
    for (name, image) in std::iter::once(("runtime", Image::Runtime)).chain(runtime.languages().iter().copied()) {
        let digests = generation
            .images()
            .get(&image)
            .ok_or_else(|| InstallError::Invalid(format!("generation has no {name} image")))?;
        images.insert(
            name,
            json!({"repository": image.repository(),
            "tag": generation.release(), "digest": digests.digest(),
            "platforms": digests.platforms()}),
        );
    }
    documents.insert(
        "images.json".to_owned(),
        json!({"schema": 1,
        "generation": generation.commit(), "registry": IMAGE_REGISTRY,
        "runtime_url": "http://appa-runtime.appa.svc.cluster.local:18787",
        "images": images}),
    );
    for (language, _) in runtime.languages() {
        let mut deployment = json!({
            "labels": {"appa.dev/managed": "kagent", "appa.dev/runtime": language},
            "env": [
                {"name": "APPA_ENABLED", "value": "true"},
                {"name": "APPA_RUNTIME_URL", "value": "http://appa-runtime.appa.svc.cluster.local:18787"}
            ]
        });
        if *language == "go" {
            deployment["nodeSelector"] = json!({"kubernetes.io/os": "linux", "kubernetes.io/arch": "amd64"});
        }
        documents.insert(
            format!("agent-{language}.json"),
            json!({"spec": {
                "type": "Declarative", "declarative": {"runtime": language, "deployment": deployment}
            }}),
        );
    }
    for (name, value) in documents {
        let mut bytes = serde_json::to_vec_pretty(&value)
            .map_err(|error| InstallError::Invalid(format!("serialize kagent artifact: {error}")))?;
        bytes.push(b'\n');
        files.insert(name, bytes);
    }
    files.insert("verify-images.py".into(), VERIFIER.as_bytes().to_vec());
    files.insert("KAGENT.md".into(), README.as_bytes().to_vec());
    Ok(files)
}

const README: &str = r#"# Prepared kagent integration

Preparation does not install Helm releases or modify Kubernetes. These settings
target ordinary Declarative Agents on kagent 0.9.12. Python and Go use the same
generation tag; that controller derives `golang-adk` beside `appa-kagent-adk`.
The controller image setting affects all its ordinary declarative agents.

Merge kagent-values.json into your kagent Helm values. Merge the selected
agent-*.json snippet into each Agent you want APPA to gate. Preserve existing
modelConfig, systemMessage, tools, credentials, labels, and unrelated env entries;
merge env by name, not by replacing the list. The snippets are not complete
Agent resources. Go requires linux/amd64 nodes. No tools or MCP servers are
registered by installing a battery.

The runtime's prepared Helm values pin its image by digest. Agent image tags
are generation-specific and publication refuses overwrites, but Kubernetes does
not enforce the locked agent digests. A mismatched image can start before the
post-deployment check detects it. Re-run verification after rollout or restarts.

Before deployment, from this directory:

    python3 verify-images.py

After deploying the runtime as `appa-runtime` in namespace `appa` and merging the
Agent snippets in your chosen namespace:

    python3 verify-images.py --context YOUR_CONTEXT --namespace YOUR_AGENT_NAMESPACE

Python 3, crane, and (for cluster checks) kubectl must be on PATH. The helper
performs only registry reads and kubectl get. It checks registry index/platform
digests, every non-terminating pod carrying the snippets' appa.dev/managed=kagent
label in that namespace, and the runtime release's pods in namespace appa. Each
selected language must have a pod. Unready pods, missing evidence, unsupported
platforms, wrong desired image/env, and unrecognized image IDs fail. Unlabelled
agents and other namespaces are outside this check; this is image verification,
not a policy-coverage or end-to-end tool-call test. It cannot prove an unobserved
future pod will run these bytes. Read access to nodes is needed for platform IDs.

For restricted networks, mirror every image and platform in images.json without
changing digests (for example using crane copy), preserving the sibling image
names and generation tag. Supply the mirror repository prefix with --registry
to the verifier, and override repository/registry in your deployment values.
Do not change images.json digests. The runtime override must retain its digest.
Mirroring is an explicit operator action; preparation does not push images or
fetch files inside pods. Registry verification can run from a connected machine;
--pods-only requires context and namespace and checks running evidence without
contacting a registry. It does not replace the separate registry check.
"#;

const VERIFIER: &str = r#"#!/usr/bin/env python3
"""Read-only, bounded verification; no deployment or digest enforcement."""
import argparse
import json
import os
import pathlib
import queue
import re
import signal
import subprocess
import sys
import threading
import time

LIMIT = 4 * 1024 * 1024
DEADLINE = None

def require(condition, message):
    if not condition:
        raise ValueError(message)

def run(argv):
    require(time.monotonic() < DEADLINE, 'verification exceeded total time budget')
    output = queue.Queue(maxsize=8)
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=(os.name == 'posix'))
    def read(stream, lane):
        try:
            while True:
                chunk = stream.read(8192)
                output.put((lane, chunk))
                if not chunk:
                    break
        finally:
            stream.close()
    for lane, stream in enumerate((process.stdout, process.stderr)):
        threading.Thread(target=read, args=(stream, lane), daemon=True).start()
    buffers = [bytearray(), bytearray()]
    end = min(DEADLINE, time.monotonic() + 30)
    finished = 0
    try:
        while finished < 2:
            require(time.monotonic() < end, 'external read timed out: ' + argv[0])
            try:
                lane, chunk = output.get(timeout=0.05)
            except queue.Empty:
                continue
            if not chunk:
                finished += 1
            else:
                require(len(buffers[lane]) + len(chunk) <= LIMIT,
                        'external output exceeds limit: ' + argv[0])
                buffers[lane].extend(chunk)
        code = process.wait(timeout=max(0.01, end - time.monotonic()))
        # Do not echo stderr: kubectl auth plugins can print credentials.
        require(code == 0, 'external read failed: ' + argv[0])
        return buffers[0].decode('utf-8')
    finally:
        if os.name == 'posix':
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        if process.poll() is None:
            process.kill()
        process.wait(timeout=2)

def reference(entry, registry):
    repository = entry['repository']
    if registry:
        repository = registry.rstrip('/') + '/' + repository.rsplit('/', 1)[1]
    return repository

def main():
    global DEADLINE
    DEADLINE = time.monotonic() + 180
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context')
    parser.add_argument('--namespace')
    parser.add_argument('--registry', help='mirror registry/repository prefix')
    parser.add_argument('--pods-only', action='store_true')
    args = parser.parse_args()
    require(bool(args.context) == bool(args.namespace), 'context and namespace are required together')
    require(not args.pods_only or args.context, 'pods-only requires context and namespace')
    if args.registry:
        require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._:/-]*', args.registry), 'invalid mirror prefix')
    with pathlib.Path(__file__).with_name('images.json').open('rb') as source:
        raw = source.read(65537)
    require(len(raw) <= 65536, 'image lock exceeds limit')
    lock = json.loads(raw)
    require(lock['schema'] == 1, 'unsupported image lock')
    images = lock['images']
    require('runtime' in images and set(images) <= {'runtime', 'python', 'go'}
            and len(images) >= 2, 'image lock has no selected language')
    for entry in images.values():
        require(re.fullmatch(r'sha256:[0-9a-f]{64}', entry['digest']), 'invalid image digest')
        require(entry['platforms'], 'missing platform digests')
        for platform, digest in entry['platforms'].items():
            require(platform in ('linux/amd64', 'linux/arm64')
                    and re.fullmatch(r'sha256:[0-9a-f]{64}', digest), 'invalid platform digest')
        repo = reference(entry, args.registry)
        require(not repo.startswith('-') and not entry['tag'].startswith('-'), 'invalid image reference')
        ref = repo + ':' + entry['tag']
        if not args.pods_only:
            require(run(['crane', 'digest', ref]).strip() == entry['digest'], 'registry digest mismatch: ' + ref)
            for platform, digest in entry['platforms'].items():
                require(run(['crane', 'digest', '--platform', platform, ref]).strip() == digest,
                        'registry platform mismatch: ' + ref + ' ' + platform)
    if args.context:
        def get(resource, namespace=None, selector=None):
            argv = ['kubectl', '--context', args.context, '--request-timeout=20s']
            if namespace:
                argv += ['--namespace', namespace]
            argv += ['get', resource, '-o', 'json']
            if selector:
                argv += ['--selector', selector]
            return json.loads(run(argv))['items']
        nodes = {node['metadata']['name']: node['status']['nodeInfo'] for node in get('nodes')}
        agents = get('pods', args.namespace, 'appa.dev/managed=kagent')
        runtime = get('pods', 'appa', 'app.kubernetes.io/instance=appa-runtime,app.kubernetes.io/name=appa-runtime')
        seen = set()
        for pod, kind in [(p, p['metadata'].get('labels', {}).get('appa.dev/runtime')) for p in agents] + [(p, 'runtime') for p in runtime]:
            if pod['metadata'].get('deletionTimestamp'):
                continue
            name = pod['metadata']['name']
            require(kind in images, 'unselected or unknown runtime on pod ' + name)
            require(pod['status'].get('phase') == 'Running' and any(
                c.get('type') == 'Ready' and c.get('status') == 'True'
                for c in pod['status'].get('conditions', [])), 'pod is not ready: ' + name)
            entry = images[kind]
            repo = reference(entry, args.registry)
            desired = repo + ('@' + entry['digest'] if kind == 'runtime' else ':' + entry['tag'])
            containers = [c for c in pod['spec']['containers'] if c.get('image') == desired]
            require(len(containers) == 1, 'missing or ambiguous expected image on pod ' + name)
            container = containers[0]
            if kind != 'runtime':
                env = container.get('env', [])
                for key, expected in [('APPA_ENABLED', 'true'), ('APPA_RUNTIME_URL', lock['runtime_url'])]:
                    matches = [v for v in env if v.get('name') == key]
                    require(len(matches) == 1 and matches[0].get('value') == expected
                            and 'valueFrom' not in matches[0], 'missing explicit ' + key + ' on ' + name)
            node = nodes.get(pod['spec'].get('nodeName'), {})
            platform = node.get('operatingSystem', '') + '/' + node.get('architecture', '')
            require(platform in entry['platforms'], 'unsupported or unknown platform on ' + name)
            statuses = [s for s in pod['status'].get('containerStatuses', []) if s.get('name') == container['name']]
            require(len(statuses) == 1 and statuses[0].get('ready') is True
                    and 'running' in statuses[0].get('state', {}), 'container is not running and ready: ' + name)
            image_id = statuses[0].get('imageID', '')
            # Accept manifest digests only. Config IDs and unknown CRI forms fail.
            match = re.fullmatch(r'(?:(?:docker-pullable|containerd)://)?(?:[^\s@]+@)?(sha256:[0-9a-f]{64})', image_id)
            require(match and match.group(1) == entry['platforms'][platform], 'running manifest digest is unknown or mismatched: ' + name)
            seen.add(kind)
        require(seen == set(images), 'no verified ready pods for: ' + ', '.join(sorted(set(images) - seen)))
    print('Verified ' + ('running image evidence only' if args.pods_only else 'registry image digests')
          + (' and selected pods' if args.context and not args.pods_only else '')
          + '; this is not digest enforcement or policy validation.')

if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
        print('Image verification failed: ' + str(error), file=sys.stderr)
        sys.exit(1)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_host_settings_lock_only_selected_languages() {
        let selected = super::super::tests::selection();
        for (runtime, expected) in [
            (KagentRuntime::Python, vec!["python"]),
            (KagentRuntime::Go, vec!["go"]),
            (KagentRuntime::Both, vec!["python", "go"]),
        ] {
            let files = artifacts(&selected.generation, runtime).unwrap();
            let lock: serde_json::Value = serde_json::from_slice(&files["images.json"]).unwrap();
            assert_eq!(lock["images"].as_object().unwrap().len(), expected.len() + 1);
            for language in expected {
                let snippet: serde_json::Value =
                    serde_json::from_slice(&files[&format!("agent-{language}.json")]).unwrap();
                assert_eq!(snippet["spec"]["declarative"]["runtime"], language);
                assert!(snippet["spec"]["declarative"].get("tools").is_none());
                assert!(snippet.get("metadata").is_none());
                if language == "go" {
                    assert_eq!(
                        snippet["spec"]["declarative"]["deployment"]["nodeSelector"]["kubernetes.io/arch"],
                        "amd64"
                    );
                    assert!(
                        lock["images"]["go"]["repository"]
                            .as_str()
                            .unwrap()
                            .ends_with("/golang-adk")
                    );
                }
            }
            let values: serde_json::Value = serde_json::from_slice(&files["kagent-values.json"]).unwrap();
            assert_eq!(
                values["controller"]["agentImage"]["registry"],
                "europe-west1-docker.pkg.dev"
            );
            assert_eq!(
                values["controller"]["agentImage"]["repository"],
                "friendly-path-465518-r6/appa-public/appa-kagent-adk"
            );
            assert_eq!(values["controller"]["agentImage"]["tag"], selected.generation.release());
        }
    }

    /// Real helper process and real PATH-resolved tools. The fixture controls
    /// their wire output, not verifier internals. Cluster behavior remains an
    /// integration-test requirement.
    #[cfg(unix)]
    mod subprocess {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Output};

        struct Fixture {
            root: tempfile::TempDir,
            evidence: serde_json::Value,
        }

        impl Fixture {
            fn new() -> Self {
                let selected = super::super::super::tests::selection();
                let root = tempfile::tempdir().unwrap();
                let artifacts = artifacts(&selected.generation, KagentRuntime::Both).unwrap();
                for (path, bytes) in &artifacts {
                    fs::write(root.path().join(path), bytes).unwrap();
                }
                let lock: serde_json::Value = serde_json::from_slice(&artifacts["images.json"]).unwrap();
                let pod = |kind: &str| {
                    let image = &lock["images"][kind];
                    let repository = image["repository"].as_str().unwrap();
                    let desired = if kind == "runtime" {
                        format!("{repository}@{}", image["digest"].as_str().unwrap())
                    } else {
                        format!("{repository}:{}", image["tag"].as_str().unwrap())
                    };
                    json!({"metadata": {"name": format!("test-{kind}"),
                        "labels": {"appa.dev/managed": "kagent", "appa.dev/runtime": kind}},
                        "spec": {"nodeName": "test-node", "containers": [{"name": "main", "image": desired,
                            "env": [{"name": "APPA_ENABLED", "value": "true"},
                                {"name": "APPA_RUNTIME_URL", "value": lock["runtime_url"]}]}]},
                        "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}],
                            "containerStatuses": [{"name": "main", "ready": true, "state": {"running": {}},
                                "imageID": format!("{repository}@{}", image["platforms"]["linux/amd64"].as_str().unwrap())}]}})
                };
                let evidence = json!({"mode": "success", "lock": lock,
                    "nodes": {"items": [{"metadata": {"name": "test-node"},
                        "status": {"nodeInfo": {"operatingSystem": "linux", "architecture": "amd64"}}}]},
                    "agents": {"items": [pod("python"), pod("go")]},
                    "runtime": {"items": [pod("runtime")]}});
                let bin = root.path().join("bin");
                fs::create_dir(&bin).unwrap();
                for name in ["crane", "kubectl"] {
                    let path = bin.join(name);
                    fs::write(&path, FAKE_TOOL).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                }
                Self { root, evidence }
            }

            fn run(&self, args: &[&str]) -> Output {
                fs::write(
                    self.root.path().join("fixture.json"),
                    serde_json::to_vec(&self.evidence).unwrap(),
                )
                .unwrap();
                let paths = std::iter::once(self.root.path().join("bin"))
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()))
                    .collect::<Vec<_>>();
                Command::new("python3")
                    .arg(self.root.path().join("verify-images.py"))
                    .args(args)
                    .env("PATH", std::env::join_paths(paths).unwrap())
                    .env("APPA_IMAGE_TEST_FIXTURE", self.root.path().join("fixture.json"))
                    .env("APPA_IMAGE_TEST_CALLS", self.root.path().join("calls.jsonl"))
                    .output()
                    .expect("image verifier subprocess tests require python3")
            }

            fn fails(&self, args: &[&str], message: &str) {
                let output = self.run(args);
                assert!(
                    !output.status.success(),
                    "unexpected success: {}",
                    String::from_utf8_lossy(&output.stdout)
                );
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains(message), "expected {message:?}, got {stderr}");
                assert!(!stderr.contains("fixture-private-credential"));
            }

            fn calls(&self) -> Vec<Vec<String>> {
                fs::read_to_string(self.root.path().join("calls.jsonl"))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            }
        }

        const PODS: &[&str] = &["--pods-only", "--context", "test-context", "--namespace", "kagent"];

        #[test]
        fn emitted_helper_checks_registry_and_each_platform_via_real_subprocesses() {
            let fixture = Fixture::new();
            let output = fixture.run(&[]);
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let calls = fixture.calls();
            assert_eq!(calls.len(), 6);
            assert!(calls.iter().all(|argv| argv[0] == "crane" && argv[1] == "digest"));
            assert_eq!(
                calls
                    .iter()
                    .filter(|argv| argv.iter().any(|arg| arg == "--platform"))
                    .count(),
                3
            );
        }

        #[test]
        fn emitted_helper_refuses_registry_and_platform_mismatch() {
            for (mode, message) in [
                ("digest-mismatch", "registry digest mismatch"),
                ("platform-mismatch", "registry platform mismatch"),
            ] {
                let mut fixture = Fixture::new();
                fixture.evidence["mode"] = json!(mode);
                fixture.fails(&[], message);
            }
        }

        #[test]
        fn emitted_helper_accepts_both_running_languages_without_registry_access() {
            let fixture = Fixture::new();
            let output = fixture.run(PODS);
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let calls = fixture.calls();
            assert_eq!(calls.len(), 3);
            assert!(calls.iter().all(|argv| argv[0] == "kubectl"
                && argv.iter().any(|arg| arg == "get")
                && argv.windows(2).any(|pair| pair == ["--context", "test-context"])));
        }

        #[test]
        fn emitted_helper_refuses_empty_missing_and_unready_running_evidence() {
            let mut fixture = Fixture::new();
            fixture.evidence["agents"]["items"] = json!([]);
            fixture.fails(PODS, "no verified ready pods for: go, python");
            let mut fixture = Fixture::new();
            fixture.evidence["agents"]["items"].as_array_mut().unwrap().pop();
            fixture.fails(PODS, "no verified ready pods for: go");
            let mut fixture = Fixture::new();
            fixture.evidence["runtime"]["items"] = json!([]);
            fixture.fails(PODS, "no verified ready pods for: runtime");
            let mut fixture = Fixture::new();
            fixture.evidence["agents"]["items"][0]["status"]["conditions"][0]["status"] = json!("False");
            fixture.fails(PODS, "pod is not ready");
            let mut fixture = Fixture::new();
            fixture.evidence["agents"]["items"][0]["status"]["containerStatuses"] = json!([]);
            fixture.fails(PODS, "container is not running and ready");
        }

        #[test]
        fn emitted_helper_refuses_unknown_or_mismatched_running_ids_and_settings() {
            for image_id in ["", "docker://sha256:012345", &format!("sha256:{}", "b".repeat(64))] {
                let mut fixture = Fixture::new();
                fixture.evidence["agents"]["items"][0]["status"]["containerStatuses"][0]["imageID"] = json!(image_id);
                fixture.fails(PODS, "running manifest digest is unknown or mismatched");
            }
            let mut fixture = Fixture::new();
            fixture.evidence["agents"]["items"][0]["spec"]["containers"][0]["image"] = json!("wrong/image:latest");
            fixture.fails(PODS, "missing or ambiguous expected image");
            let mut fixture = Fixture::new();
            fixture.evidence["agents"]["items"][0]["spec"]["containers"][0]["env"][0]["value"] = json!("false");
            fixture.fails(PODS, "missing explicit APPA_ENABLED");
            let mut fixture = Fixture::new();
            fixture.evidence["nodes"]["items"][0]["status"]["nodeInfo"]["architecture"] = json!("unknown");
            fixture.fails(PODS, "unsupported or unknown platform");
        }

        #[test]
        fn emitted_helper_bounds_external_output_and_refuses_failures_and_invalid_json() {
            for (mode, message) in [
                ("excess-stdout", "external output exceeds limit"),
                ("excess-stderr", "external output exceeds limit"),
                ("failure", "external read failed"),
            ] {
                let mut fixture = Fixture::new();
                fixture.evidence["mode"] = json!(mode);
                fixture.fails(&[], message);
            }
            let mut fixture = Fixture::new();
            fixture.evidence["mode"] = json!("invalid-json");
            fixture.fails(PODS, "Image verification failed");
            let fixture = Fixture::new();
            fs::write(fixture.root.path().join("images.json"), b"not json").unwrap();
            fixture.fails(&[], "Image verification failed");
            assert!(!fixture.root.path().join("calls.jsonl").exists());
        }

        #[test]
        fn emitted_helper_requires_explicit_cluster_target() {
            let fixture = Fixture::new();
            fixture.fails(&["--pods-only"], "pods-only requires context and namespace");
            fixture.fails(
                &["--context", "test-context"],
                "context and namespace are required together",
            );
            assert!(!fixture.root.path().join("calls.jsonl").exists());
        }

        const FAKE_TOOL: &str = r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['APPA_IMAGE_TEST_CALLS'], 'a') as calls:
    calls.write(json.dumps([tool] + args) + '\n')
with open(os.environ['APPA_IMAGE_TEST_FIXTURE']) as source:
    fixture = json.load(source)
mode = fixture['mode']
if mode in ('excess-stdout', 'excess-stderr'):
    stream = sys.stdout if mode == 'excess-stdout' else sys.stderr
    stream.write('x' * (4 * 1024 * 1024 + 8192))
    sys.exit(0)
if mode == 'failure':
    print('fixture-private-credential', file=sys.stderr)
    sys.exit(17)
if tool == 'crane':
    assert args[0] == 'digest'
    image_name = args[-1].rsplit('/', 1)[1].split(':', 1)[0]
    kind = {'appa-runtime': 'runtime', 'appa-kagent-adk': 'python', 'golang-adk': 'go'}[image_name]
    entry = fixture['lock']['images'][kind]
    platform = args[args.index('--platform') + 1] if '--platform' in args else None
    if (mode == 'digest-mismatch' and not platform) or (mode == 'platform-mismatch' and platform):
        print('sha256:' + 'b' * 64)
    else:
        print(entry['platforms'][platform] if platform else entry['digest'])
elif tool == 'kubectl':
    assert args[args.index('--context') + 1] == 'test-context'
    assert args[args.index('--request-timeout=20s') + 1] in ('get', '--namespace')
    resource = args[args.index('get') + 1]
    if mode == 'invalid-json':
        print('{broken json')
    elif resource == 'nodes':
        print(json.dumps(fixture['nodes']))
    else:
        assert resource == 'pods'
        namespace = args[args.index('--namespace') + 1]
        selector = args[args.index('--selector') + 1]
        if namespace == 'appa':
            assert selector == 'app.kubernetes.io/instance=appa-runtime,app.kubernetes.io/name=appa-runtime'
            print(json.dumps(fixture['runtime']))
        else:
            assert namespace == 'kagent' and selector == 'appa.dev/managed=kagent'
            print(json.dumps(fixture['agents']))
else:
    raise AssertionError('unexpected executable')
"#;
    }
}
