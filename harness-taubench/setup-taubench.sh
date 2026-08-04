#!/usr/bin/env bash
set -euo pipefail

readonly REPOSITORY="https://github.com/sierra-research/tau2-bench.git"
readonly REVISION="93ee97b8303ce0e89e0ad17e6207591a1846f84b"
readonly HARNESS_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly CHECKOUT_DIR="${HARNESS_DIR}/.tau2-bench"

if [[ ! -e "${CHECKOUT_DIR}" ]]; then
    git clone "${REPOSITORY}" "${CHECKOUT_DIR}"
elif [[ ! -d "${CHECKOUT_DIR}/.git" ]]; then
    echo "error: ${CHECKOUT_DIR} exists but is not a Git checkout" >&2
    exit 1
fi

if [[ -n "$(git -C "${CHECKOUT_DIR}" status --porcelain --untracked-files=no)" ]]; then
    echo "error: ${CHECKOUT_DIR} has tracked changes; preserve or discard them before rerunning setup" >&2
    exit 1
fi

if ! git -C "${CHECKOUT_DIR}" cat-file -e "${REVISION}^{commit}" 2>/dev/null; then
    git -C "${CHECKOUT_DIR}" fetch origin "${REVISION}"
fi
git -C "${CHECKOUT_DIR}" checkout --detach "${REVISION}"

uv sync --project "${HARNESS_DIR}" --locked

required=(srt rg)
if [[ "$(uname -s)" == "Linux" ]]; then
    required+=(bwrap socat)
fi
missing=()
for executable in "${required[@]}"; do
    if ! command -v "${executable}" >/dev/null 2>&1; then
        missing+=("${executable}")
    fi
done
if (( ${#missing[@]} > 0 )); then
    echo "error: missing Tau Knowledge sandbox executables: ${missing[*]}" >&2
    echo "install @anthropic-ai/sandbox-runtime@0.0.23 and the platform packages documented in README.md" >&2
    exit 1
fi

echo "Tau Knowledge is installed at ${CHECKOUT_DIR}"
echo "Set the required API key, then run: uv run --project ${HARNESS_DIR} appa-taubench preflight"
