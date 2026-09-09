#!/usr/bin/env python3
"""Isolated amd64 acceptance of installer output, native agents and an OCI mirror.

Requires prebuilt appa-acceptance-{runtime,python,go,fixtures}:ci images.
Creates only uniquely named test resources and keeps logs in a new directory.
No cloud/model credentials are used. Never reads the default kubeconfig.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

REPO = Path(__file__).resolve().parents[4]
LIMIT = 4 * 1024 * 1024


def require(value, message):
    if not value:
        raise RuntimeError(message)


def http(url, body=None):
    request = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=180) as response:
        data = response.read(LIMIT + 1)
    require(len(data) <= LIMIT, "HTTP acceptance response exceeds limit")
    return json.loads(data)


def denial_count(value):
    if isinstance(value, str):
        try:
            return denial_count(json.loads(value))
        except (ValueError, RecursionError):
            # The Python ADK OpenAI adapter unwraps the plugin's result field.
            return int(value.startswith("[appa] Blocked:"))
    if isinstance(value, list):
        return sum(denial_count(item) for item in value)
    if isinstance(value, dict):
        return int(value.get("appa") == "denied") + sum(denial_count(item) for item in value.values())
    return 0


def assert_evidence(state, steps, reads, writes, denials, read_tool="get_file_contents"):
    requests = state["requests"]
    require([request["index"] for request in requests] == list(range(steps)), "model did not execute the complete test script")
    calls = state["invocations"]
    require(sum(call["tool"] == read_tool for call in calls) == reads, "wrong real MCP read count")
    require(sum(call["tool"] == "issue_write" for call in calls) == writes, "wrong real MCP write count")
    feedback = [message.get("content") for message in requests[-1]["messages"] if message.get("role") == "tool"]
    require(sum(denial_count(message) > 0 for message in feedback) == denials, "expected policy refusals are absent from actual tool feedback")


class Acceptance:
    image_tag = "ci"
    read_tool = "get_file_contents"

    def read_call(self):
        return {"tool": "get_file_contents", "args": {"owner": "acme", "repo": "public", "path": "README.md"}}

    def source_config(self):
        return (REPO / "marketplace/plugins/kagent/default.appa.toml").read_bytes()

    def install_batteries(self, appa, deployed, server):
        return json.loads(self.command([appa, "battery", "install", "github", "--config", deployed, "--server", server, "--json"]).splitlines()[-1])

    def __init__(self, work):
        self.work = work.resolve()
        self.work.mkdir(parents=True, exist_ok=False)
        self.name = "appa-marketplace-" + uuid.uuid4().hex[:12]
        self.registry = self.name + "-registry"
        self.context = "kind-" + self.name
        self.env = dict(os.environ, KUBECONFIG=str(self.work / "kubeconfig"))
        self.cluster_created = False
        self.registry_created = False
        self.forwards = []
        self.command_number = 0

    def command(self, args, data=None, timeout=180, expect_success=True):
        self.command_number += 1
        log = self.work / f"command-{self.command_number:03}.log"
        print("+", " ".join(map(str, args)), flush=True)
        with log.open("wb") as output:
            process = subprocess.Popen(list(map(str, args)), cwd=REPO, env=self.env,
                                       stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                                       stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                process.communicate(None if data is None else data.encode(), timeout=timeout)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
        content = log.read_bytes()
        require(len(content) <= 16 * LIMIT, f"command output too large: {log}")
        if expect_success:
            require(process.returncode == 0, f"command failed ({process.returncode}): {log}\n{content[-16000:].decode(errors='replace')}")
        else:
            require(process.returncode != 0, f"expected refusal, command succeeded: {log}")
        return content.decode()

    def kubectl(self, *args, **kwargs):
        return self.command(["kubectl", "--kubeconfig", self.env["KUBECONFIG"], "--context", self.context, *args], **kwargs)

    def helm(self, *args):
        return self.command(["helm", *args, "--kube-context", self.context], timeout=600)

    def apply(self, resources):
        self.kubectl("apply", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "List", "items": resources}))

    def forward(self, name, port):
        log = self.work / f"forward-{name}-{port}-{len(self.forwards)}.log"
        output = log.open("wb")
        process = subprocess.Popen(["kubectl", "--kubeconfig", self.env["KUBECONFIG"], "--context", self.context,
                                    "-n", "kagent", "port-forward", "--address", "127.0.0.1", "svc/" + name, f"0:{port}"],
                                   stdout=output, stderr=subprocess.STDOUT, env=self.env)
        self.forwards.append((process, output))
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            require(process.poll() is None, f"port forward failed: {log}")
            for line in log.read_text().splitlines():
                if line.startswith("Forwarding from 127.0.0.1:"):
                    return "http://" + line.split()[2]
            time.sleep(0.1)
        raise RuntimeError(f"port forward timed out: {log}")

    def run(self):
        info = json.loads(self.command(["docker", "info", "--format", "{{json .}} "]))
        require(info["Architecture"] in ("x86_64", "amd64"), "both-language acceptance requires an amd64 Docker host; no ARM relabelling")
        for kind in ("runtime", "python", "go", "fixtures"):
            self.command(["docker", "image", "inspect", f"appa-acceptance-{kind}:{self.image_tag}"])
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        mirror = f"localhost:{port}"
        self.command(["docker", "create", "--name", self.registry, "-p", f"127.0.0.1:{port}:5000", "registry:3"])
        self.registry_created = True
        self.command(["docker", "start", self.registry])
        deadline = time.monotonic() + 15
        while True:
            try:
                http(f"http://{mirror}/v2/")
                break
            except (OSError, ValueError):
                require(time.monotonic() < deadline, "test registry did not become ready")
                time.sleep(0.1)
        commit = self.command(["git", "rev-parse", "HEAD"]).strip()
        release = "v0.0.0-acceptance." + commit[:12]
        names = {"runtime": "appa-runtime", "python": "appa-kagent-adk", "go": "golang-adk", "fixtures": "fixtures"}
        images = {}
        for kind, repository in names.items():
            ref = f"{mirror}/{repository}:{release}"
            self.command(["docker", "tag", f"appa-acceptance-{kind}:{self.image_tag}", ref])
            self.command(["docker", "push", ref], timeout=600)
            if kind != "fixtures":
                digest = self.command(["crane", "digest", ref]).strip()
                platform = self.command(["crane", "digest", "--platform", "linux/amd64", ref]).strip()
                images[kind] = {"digest": digest, "platforms": {"linux/amd64": platform}}
        self.command(["helm", "package", "charts/appa-runtime", "--version", release[1:], "--app-version", release[1:], "--destination", self.work])
        chart = self.work / f"appa-runtime-{release[1:]}.tgz"
        placeholder = "sha256:" + hashlib.sha256(b"unused acceptance artifact").hexdigest()
        platforms = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
        descriptor = {"schema": 1, "repository": "archestra-ai/OpenAPPA", "commit": commit,
                      "release": release, "protocol": 1, "catalog": placeholder, "marketplace": placeholder,
                      "claude_plugin": placeholder, "runtime_chart": "sha256:" + hashlib.sha256(chart.read_bytes()).hexdigest(),
                      "binaries": {p: placeholder for p in platforms}, "images": images}
        source = self.work / "descriptor.json"
        source.write_text(json.dumps(descriptor))
        config = self.work / "source.toml"
        config.write_bytes(self.source_config())
        bundle = self.work / "fixture.tar.gz"
        fixture = json.loads(self.command(["cargo", "run", "--quiet", "--locked", "-p", "appa", "--example", "kagent_installation_fixture", "--", source, config, chart, bundle], timeout=600).splitlines()[-1])
        self.command(["cargo", "build", "--locked", "-p", "appa", "--bin", "appa"], timeout=600)
        appa = REPO / "target/debug/appa"
        deployed = self.work / "deployment/appa.toml"
        self.command([appa, "plugin", "install", "kagent", "--config", deployed, "--from", bundle, "--sha256", fixture["sha256"], "--json"])
        endpoint = "http://marketplace-fixtures.kagent.svc.cluster.local:3000/mcp"
        server = "server-" + hashlib.sha256(endpoint.encode()).hexdigest()
        battery = self.install_batteries(appa, deployed, server)
        prepared = Path(battery["result"]["directory"])
        self.command([sys.executable, prepared / "verify-images.py", "--registry", mirror])
        # Refusal is tested against a copy; installed owned state is unchanged.
        mismatch = self.work / "mismatch"
        mismatch.mkdir()
        (mismatch / "verify-images.py").write_bytes((prepared / "verify-images.py").read_bytes())
        wrong = json.loads((prepared / "images.json").read_text())
        wrong["images"]["go"]["digest"] = placeholder
        (mismatch / "images.json").write_text(json.dumps(wrong))
        self.command([sys.executable, mismatch / "verify-images.py", "--registry", mirror], expect_success=False)
        self.cluster_created = True
        cluster_config = {
            "kind": "Cluster", "apiVersion": "kind.x-k8s.io/v1alpha4",
            "containerdConfigPatches": ['[plugins."io.containerd.grpc.v1.cri".registry]\n  config_path = "/etc/containerd/certs.d"\n'],
        }
        # This pinned node does not enable the CRI registry directory by default.
        node_image = "kindest/node:v1.32.2@sha256:f226345927d7e348497136874b6d207e0b32cc52154ad8323129352923a3142f"
        self.command(["kind", "create", "cluster", "--name", self.name, "--kubeconfig", self.env["KUBECONFIG"],
                      "--image", node_image, "--config", "-", "--wait", "180s"], data=json.dumps(cluster_config), timeout=300)
        self.command(["docker", "network", "connect", "kind", self.registry])
        for node in self.command(["kind", "get", "nodes", "--name", self.name]).splitlines():
            directory = f"/etc/containerd/certs.d/{mirror}"
            self.command(["docker", "exec", node, "mkdir", "-p", directory])
            self.command(["docker", "exec", "-i", node, "cp", "/dev/stdin", directory + "/hosts.toml"], data=f'[host."http://{self.registry}:5000"]\n')
            self.command(["docker", "exec", node, "crictl", "pull", f"{mirror}/appa-runtime@{images['runtime']['digest']}"], timeout=180)
        version = "0.9.12"
        self.helm("upgrade", "--install", "kagent-crds", "oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds", "--version", version, "-n", "kagent", "--create-namespace", "--wait")
        extras = ("k8s-agent", "kgateway-agent", "istio-agent", "promql-agent", "observability-agent", "argo-rollouts-agent", "helm-agent", "cilium-policy-agent", "cilium-manager-agent", "cilium-debug-agent", "grafana-mcp", "querydoc", "kagent-tools")
        flags = [flag for extra in extras for flag in ("--set", extra + ".enabled=false")]
        self.helm("upgrade", "--install", "kagent", "oci://ghcr.io/kagent-dev/kagent/helm/kagent", "--version", version, "-n", "kagent", "-f", prepared / "kagent-values.json", "--set-string", "controller.agentImage.registry=" + mirror,
                  "--set-string", "controller.agentImage.repository=appa-kagent-adk", "--set", "ui.replicas=0", *flags, "--wait", "--timeout", "8m")
        self.deploy_runtime(prepared, chart.name, mirror)
        self.command([sys.executable, prepared / "verify-images.py", "--registry", mirror, "--context", self.context, "--namespace", "kagent", "--pods-only"], expect_success=False)
        self.apply(self.resources(prepared, mirror, release, endpoint))
        for name in ("marketplace-fixtures", "marketplace-python", "marketplace-go"):
            deadline = time.monotonic() + 300
            while time.monotonic() < deadline:
                result = json.loads(self.kubectl("-n", "kagent", "get", "deployments", "-o", "json"))
                if any(item["metadata"]["name"] == name for item in result["items"]):
                    break
                time.sleep(2)
            self.kubectl("-n", "kagent", "rollout", "status", "deployment/" + name, "--timeout=300s", timeout=330)
        self.command([sys.executable, prepared / "verify-images.py", "--registry", mirror, "--context", self.context, "--namespace", "kagent"])
        fixture_url = self.forward("marketplace-fixtures", 8080)
        for language in ("python", "go"):
            self.scenarios(self.forward("marketplace-" + language, 8080), fixture_url, language)
        archive = self.work / "exported.tar.gz"
        exported = json.loads(self.command([appa, "bundle", "--config", deployed, "--output", archive, "--json"]).splitlines()[-1])
        offline = self.work / "offline/replica.toml"
        self.command(["docker", "stop", self.registry])
        original_env = self.env
        self.env = dict(self.env, HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", NO_PROXY="")
        try:
            imported = json.loads(self.command([appa, "plugin", "install", "kagent", "--config", offline, "--from", archive, "--sha256", exported["result"]["sha256"], "--json"]).splitlines()[-1])
        finally:
            self.env = original_env
        replica = Path(imported["result"]["directory"])
        self.deploy_runtime(replica, chart.name, mirror)
        for language in ("python", "go"):
            self.kubectl("-n", "kagent", "rollout", "restart", "deployment/marketplace-" + language)
            self.kubectl("-n", "kagent", "rollout", "status", "deployment/marketplace-" + language, "--timeout=180s", timeout=210)
        self.command([sys.executable, replica / "verify-images.py", "--registry", mirror, "--context", self.context, "--namespace", "kagent", "--pods-only"])
        for language in ("python", "go"):
            self.scenarios(self.forward("marketplace-" + language, 8080), fixture_url, "offline-" + language)
        for language in ("python", "go"):
            self.approval_scenarios("marketplace-" + language, fixture_url)
        (self.work / "result.json").write_text(json.dumps({"status": "passed", "generation": commit, "languages": ["python", "go"], "offline_registry_stopped": True}))

    def deploy_runtime(self, prepared, chart, mirror):
        self.helm("upgrade", "--install", "appa-runtime", prepared / chart, "-n", "appa", "--create-namespace", "-f", prepared / "runtime-values.json", "--set-string", "image.repository=" + mirror + "/appa-runtime", "--wait", "--timeout", "5m")

    def resources(self, prepared, mirror, release, endpoint):
        def resource(kind, name, spec, api="v1"):
            return {"apiVersion": api, "kind": kind, "metadata": {"name": name, "namespace": "kagent"}, "spec": spec}
        fixture = "marketplace-fixtures"
        labels = {"app": fixture}
        resources = [resource("Deployment", fixture, {"selector": {"matchLabels": labels}, "template": {"metadata": {"labels": labels}, "spec": {"containers": [{"name": "fixtures", "image": f"{mirror}/fixtures:{release}", "ports": [{"containerPort": 8080}, {"containerPort": 3000}]}]}}}, "apps/v1"),
                     resource("Service", fixture, {"selector": labels, "ports": [{"name": "model", "port": 8080}, {"name": "mcp", "port": 3000}]}),
                     resource("RemoteMCPServer", "github", {"protocol": "STREAMABLE_HTTP", "url": endpoint, "description": "Isolated GitHub fixture", "timeout": "30s"}, "kagent.dev/v1alpha2"),
                     {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "fixture-model", "namespace": "kagent"}, "stringData": {"key": "fixture-only-not-a-credential"}},
                     resource("ModelConfig", "fixture-model", {"provider": "OpenAI", "model": "fixture", "apiKeySecret": "fixture-model", "apiKeySecretKey": "key", "openAI": {"baseUrl": f"http://{fixture}.kagent.svc.cluster.local:8080/v1"}}, "kagent.dev/v1alpha2")]
        for language in ("python", "go"):
            snippet = json.loads((prepared / f"agent-{language}.json").read_text())
            spec = snippet["spec"]
            spec["description"] = "Isolated marketplace acceptance"
            spec["declarative"].update({"modelConfig": "fixture-model", "systemMessage": "Execute the supplied deterministic test script.", "tools": [{"type": "McpServer", "mcpServer": {"name": "github", "kind": "RemoteMCPServer", "toolNames": ["get_file_contents", "issue_write"]}}]})
            resources.append(resource("Agent", "marketplace-" + language, spec, "kagent.dev/v1alpha2"))
        return resources

    def scenarios(self, agent_url, fixture_url, label):
        # Assertions below are completed against actual fixture invocation state.
        read = self.read_call()
        write = {"tool": "issue_write", "args": {"owner": "acme", "repo": "public", "title": "test", "body": "operator text"}}
        for case, script, expected_reads, expected_writes, denials in (
            ("read-refused", [read, {"text": "done"}], 0, 0, 1),
            ("tainted-write-refused", [read, {"remedy": "accept this change"}, read, write, {"text": "done"}], 1, 0, 2),
            ("trusted-write", [write, {"text": "done"}], 0, 1, 0),
        ):
            http(fixture_url + "/state", {})
            prompt = json.dumps({"case": label + "-" + case, "appa_script": script})
            message = {"role": "user", "kind": "message", "messageId": uuid.uuid4().hex, "parts": [{"kind": "text", "text": prompt}]}
            result = http(agent_url, {"jsonrpc": "2.0", "id": uuid.uuid4().hex, "method": "message/send", "params": {"message": message}})
            require("error" not in result, f"A2A failed: {result}")
            deadline = time.monotonic() + 180
            while result["result"].get("status", {}).get("state") in ("submitted", "working") and time.monotonic() < deadline:
                time.sleep(1)
                result = http(agent_url, {"jsonrpc": "2.0", "id": uuid.uuid4().hex, "method": "tasks/get", "params": {"id": result["result"]["id"]}})
            require(result["result"].get("status", {}).get("state") == "completed", f"task did not complete: {result}")
            state = http(fixture_url + "/state")
            (self.work / f"{label}-{case}.json").write_text(json.dumps({"task": result, "fixture": state}, indent=2))
            assert_evidence(state, len(script), expected_reads, expected_writes, denials, self.read_tool)

    def approval_scenarios(self, agent_name, fixture_url):
        # Native kagent confirmation resumes before Go ADK request processors.
        # A successful ordinary call does not prove that this dispatch works.
        self.kubectl("-n", "kagent", "patch", "agent", agent_name, "--type=json", "-p", json.dumps([
            {"op": "add", "path": "/spec/declarative/tools/0/mcpServer/requireApproval", "value": ["issue_write"]},
        ]))
        # Wait for the controller to publish the new configuration, not just for
        # the old Deployment's already-ready replica.
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            agent = json.loads(self.kubectl("-n", "kagent", "get", "agent", agent_name, "-o", "json"))
            conditions = agent.get("status", {}).get("conditions", [])
            if any(c.get("type") == "Ready" and c.get("status") == "True" and
                   c.get("observedGeneration") == agent["metadata"]["generation"] for c in conditions):
                break
            time.sleep(1)
        else:
            raise RuntimeError("approval configuration was not reconciled")
        self.kubectl("-n", "kagent", "rollout", "status", "deployment/" + agent_name, "--timeout=180s", timeout=210)
        agent_url = self.forward(agent_name, 8080)

        def send(message):
            result = http(agent_url, {"jsonrpc": "2.0", "id": uuid.uuid4().hex, "method": "message/send", "params": {"message": message}})
            deadline = time.monotonic() + 180
            while result.get("result", {}).get("status", {}).get("state") in ("submitted", "working") and time.monotonic() < deadline:
                time.sleep(1)
                result = http(agent_url, {"jsonrpc": "2.0", "id": uuid.uuid4().hex, "method": "tasks/get", "params": {"id": result["result"]["id"]}})
            return result

        for decision in ("reject", "approve"):
            http(fixture_url + "/state", {})
            script = [{"tool": "issue_write", "args": {"owner": "acme", "repo": "public", "title": "approval test", "body": "operator text"}}, {"text": "done"}]
            pending = send({"role": "user", "kind": "message", "messageId": uuid.uuid4().hex,
                            "parts": [{"kind": "text", "text": json.dumps({"appa_script": script})}]})
            require(pending.get("result", {}).get("status", {}).get("state") == "input-required", f"missing confirmation: {pending}")
            require(not http(fixture_url + "/state")["invocations"], "write executed before approval")
            task = pending["result"]
            result = send({"role": "user", "kind": "message", "messageId": uuid.uuid4().hex,
                           "taskId": task["id"], "contextId": task["contextId"],
                           "parts": [{"kind": "data", "data": {"decision_type": decision}}]})
            state = http(fixture_url + "/state")
            (self.work / f"{agent_name}-approval-{decision}.json").write_text(json.dumps({"pending": pending, "task": result, "fixture": state}, indent=2))
            require(result.get("result", {}).get("status", {}).get("state") == "completed", f"confirmation did not complete: {result}")
            require(state["counts"] == {"get_file_contents": 0, "issue_write": int(decision == "approve")}, "wrong actual execution count after confirmation")

    def close(self):
        errors = []
        def cleanup(args, timeout=180):
            try:
                self.command(args, timeout=timeout)
            except Exception as error:
                errors.append(str(error))
        for process, output in self.forwards:
            try:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            except OSError as error:
                errors.append(str(error))
            finally:
                output.close()
        if self.cluster_created:
            diagnostics = [("get", "pods", "-A", "-o", "wide"),
                           ("get", "events", "-A", "--sort-by=.metadata.creationTimestamp")]
            for namespace, selector in (("appa", "app.kubernetes.io/instance=appa-runtime"),
                                        ("kagent", "appa.dev/managed=kagent")):
                diagnostics.extend([
                    ("-n", namespace, "describe", "pods", "--selector=" + selector),
                    ("-n", namespace, "logs", "--all-containers", "--prefix", "--tail=200", "--selector=" + selector),
                ])
            for args in diagnostics:
                try:
                    self.kubectl(*args, timeout=30)
                except Exception as error:
                    print("Diagnostics incomplete:", error, file=sys.stderr)
            cleanup(["kind", "delete", "cluster", "--name", self.name])
        if self.registry_created:
            cleanup(["docker", "rm", "-f", self.registry])
        if errors:
            (self.work / "cleanup-errors.json").write_text(json.dumps(errors))
            if sys.exc_info()[0] is None:
                raise RuntimeError("acceptance cleanup failed; see cleanup-errors.json")
            print("Acceptance cleanup failures:", errors, file=sys.stderr)


if __name__ == "__main__":
    require(len(sys.argv) == 2, "usage: run.py <new-log-directory>")
    acceptance = Acceptance(Path(sys.argv[1]))
    try:
        acceptance.run()
    finally:
        acceptance.close()
