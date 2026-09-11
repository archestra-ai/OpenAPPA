# Isolated file processing

`appa_process_files(input_paths, output_path, command)` runs a shell command on
declared file snapshots and publishes one output. It is an opt-in Linux backend
for the Claude Code plugin, not confinement of Claude Code itself.

## Install the host-owned backend

The build requires Go, a C compiler, `pkg-config` and `libseccomp-dev`. Execution
requires `/usr/bin/python3`, `/usr/bin/bwrap`, user/PID/network namespaces,
Landlock and seccomp notification. The acceptance tests also use `keyctl`.

```sh
bash integrations/agentsh/build.sh /absolute/new/backend
python3 -m unittest integrations/agentsh/test_run.py -v
python3 integrations/agentsh/test_live.py /absolute/new/backend -v
```

The build pins agentsh v0.20.5 source and applies `fail-closed.patch`. The patch
makes required helper/socket setup, unavailable Landlock, invalid syscall
configuration and unresolved file paths fail closed. Landlock application errors
stop the child. This is a locally patched build, not an upstream agentsh release.

Keep the backend, policy and databases outside the managed workspace. Treat the
backend and the mounted system toolchain (`/usr`, `/bin`, `/lib`, `/lib64`) as
trusted, public host inputs. Do not put credentials or private data in that
toolchain. The agent must not be able to change host execution controls.

Start the file runtime with its normal workspace/ledger options and additionally:

```sh
appa runtime --config /host/policy.toml --db /host/runtime.db \
  --file-workspace /host/work --file-ledger /host/files.db \
  --file-process-backend /absolute/new/backend
```

The workspace must already be initialized using the file runtime's explicit host
classification flags. Declare `mcp/plugin_appa-runtime_appa/appa_process_files`
in the policy. The tool is absent from MCP unless the backend is enabled.

## Execution and publication contract

1. The runtime binds the tool call to the host-owned trajectory. The ledger pins
   all inputs and the destination under one durable workspace reservation.
2. The engine combines the receiving trajectory Label, tool delta and every input
   Label. This applies to output content, stdout, stderr and failures. Requirements
   and narrowing are checked before the command runs. Ignoring an input does not
   remove its Label contribution. Destination history does not taint new bytes.
3. The runtime copies the declared inputs into private staging. The child reads
   `inputs/<workspace-relative-path>` and writes `output/result`. The live workspace
   is never mounted. Input paths must be distinct and exclude the output path.
4. The runner clears the environment and closes inherited descriptors. Bubblewrap
   supplies a private filesystem and user/PID/network namespaces. Patched agentsh
   supplies Landlock and a syscall policy denying sockets, keyrings, cross-process
   memory/signals and namespace changes. It uses server-owned execution, not `wrap`.
5. The runner waits for namespace PID 1 to exit. Its descendants are torn down
   before the runtime imports output. A successful command must leave one regular,
   singly linked `output/result`; symlinks, directories and special files are refused.
   Extra staged files are discarded. Child metadata is not imported.
6. The runtime stages and atomically replaces the destination, verifies the ledger
   pins, records all input dependencies, and admits the result before returning it
   through MCP. A failed command publishes nothing. Uncertain ledger publication
   retains the reservation and requires operator reconciliation.

The command timeout is 120 seconds. A launcher timeout or malformed response never
publishes output. Command failures retain their captured diagnostic text with the
combined Label; launcher failures return a generic error. Commands start in a fresh
workspace on every call and cannot persist scratch files into the next call.

## Verified scope and exclusions

Tests cover allowed processing, input immutability, control-file denial, socket and
keyring denial, inherited descriptors, parent-memory access, detached-child teardown,
and failure handling. Runtime tests cover narrowing, success/failure Labels, absolute
input paths, binary output import, dependency persistence and quarantine. Live Claude
tests exercise a two-input invoice calculation and actual denied-access attempts.

Native Claude tools, implicit reads, inference requests and final responses are not
confined by this backend. Native Bash is still refused; shell commands must use the
Process tool. This is conservative whole-command taint, not precise dependencies
inside programs. No sanitization or declassification is supported. Resource exhaustion,
kernel vulnerabilities, metadata/timing flows and automatic crash recovery are not
covered. Run the acceptance probes on the deployment host; a capability score alone
does not establish enforcement.
