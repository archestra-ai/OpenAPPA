# Codex proxy compatibility

## Probe

Build `appa` with `cargo build -p appa`.
Run `python3 integrations/codex/proxy_probe.py`.

The probe uses a temporary project and a temporary Codex profile. It makes no model call.
It checks the rewritten wrapper, the admitted result, early output, and a forged job handle.

## Scope

The proxy supports finite Unix commands without later input. It starts the selected `sh`, `bash`, or `zsh` login shell without a controlling terminal.
It closes child stdin and buffers at most 1 MiB of combined output. It checks job status while the child runs.

Native Windows commands, later input, terminal jobs, and unsupported shell modes remain outside this proxy scope.
The protected launcher requires a Codex sandbox profile with the disclosed `127.0.0.1` HTTP exception.
The hook cannot guarantee coverage if Codex skips or disables it.

## Later input on Codex CLI 0.159.2

Tests ran on macOS 26.6.2 arm64 on 30 September 2026.
A direct `codex sandbox` command received input through a FIFO without echo.
A disposable model-controlled Codex session then ran the rewritten APPA wrapper.
Its non-TTY command received EOF before later input arrived.
Codex returned `write_stdin failed: stdin is closed for this session; rerun exec_command with tty=true to keep stdin open`.
The terminal retry echoed the synthetic `HELLO` input.

This release did not provide a non-echoing pipe for later input in the tested command session.
The wrapper keeps child stdin closed. The reserved `host/codex/appa_stdin` policy stays inactive.

## Lifetime and output

A runtime restart invalidates live job handles. Persistent ownership prevents replay.
The runtime withholds a post-hook after restart because it cannot verify the old turn.
Stop, Interrupt, and a new prompt close live jobs and block late post-hook acknowledgement.

On cancellation, the wrapper kills the shell process group and ordinary descendants.
A deliberately daemonized child can escape that group and continue after the wrapper settles indeterminately.
Codex CLI 0.159.0 also left such a child alive after its parent exited.
Do not use this proxy to contain daemonized commands. The policy must deny them until process-tree containment exists.
The wrapper withholds output from an escaped child that retains its pipes. It cannot reverse that child's side effects.
