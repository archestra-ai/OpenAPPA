#!/usr/bin/env bash
# zona's provision hook: installs OpenAPPA's pinned toolchains in the zona VM
# with mise, as mise.toml and mise.lock name them. Runs as root, in the VM
# copy, on the first `zona start` of OpenAPPA in a VM and again on
# `zona provision`. Safe to run again.
#
# Not .agents/setup: that one wires cloud credentials into the login profile,
# and the VM holds none. gcloud and claude from mise.toml are left out for the
# same reason: no cloud or Claude login in the VM. Rust comes from rustup's
# stable channel, as CI installs it: the locked dependencies need a newer rustc
# than mise.toml pins.
set -euo pipefail

MISE_VERSION=v2026.11.14
TOOLS="node python go bun uv pnpm claude"
user="${SUDO_USER:?zona runs this with sudo from the VM user}"
repo="$ZONA_DIR/.."

echo "==> apt packages for native crates and Python builds"
packages="build-essential pkg-config cmake clang libssl-dev curl xz-utils unzip"
# shellcheck disable=SC2086
if ! dpkg -s $packages >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -q
  apt-get install -y -q --no-install-recommends $packages
fi

echo "==> mise ${MISE_VERSION}"
if [ "$(mise --version 2>/dev/null | awk '{print "v" $1}')" != "$MISE_VERSION" ]; then
  # The installer checks the release's checksum.
  curl --proto '=https' --tlsv1.2 -fsSL https://mise.run |
    MISE_VERSION="$MISE_VERSION" MISE_INSTALL_PATH=/usr/local/bin/mise MISE_QUIET=1 sh
fi

# mise's shims for login shells, which is how zona runs every command. A shim
# resolves the version for the folder it runs in; in a folder no mise config
# covers (another project's copy) it falls through to the next tool on PATH.
cat > /etc/profile.d/zona-openappa.sh <<'EOF'
case ":$PATH:" in *":$HOME/.local/share/mise/shims:"*) ;; *) PATH="$HOME/.local/share/mise/shims:$PATH" ;; esac
case ":$PATH:" in *":$HOME/.cargo/bin:"*) ;; *) [ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin" ;; esac
EOF

echo "==> Rust stable with clippy and rustfmt, as CI"
if ! sudo -iu "$user" bash -c 'test -x ~/.cargo/bin/rustup'; then
  case "$(uname -m)" in aarch64 | arm64) triple=aarch64-unknown-linux-gnu ;; *) triple=x86_64-unknown-linux-gnu ;; esac
  url="https://static.rust-lang.org/rustup/dist/${triple}/rustup-init"
  tmp="$(mktemp -d)"
  curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/rustup-init" "$url"
  echo "$(curl --proto '=https' --tlsv1.2 -fsSL "$url.sha256" | awk '{print $1}')  $tmp/rustup-init" | sha256sum -c --quiet -
  install -m 0755 "$tmp/rustup-init" /tmp/rustup-init && rm -rf "$tmp"
  sudo -iu "$user" /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none
  rm -f /tmp/rustup-init
fi
sudo -iu "$user" bash -lc '~/.cargo/bin/rustup toolchain install stable --profile minimal --component clippy,rustfmt && ~/.cargo/bin/rustup default stable'
# Installed by an earlier run or a mise task: rust would shadow rustup's with
# the older pin, and gcloud needs a cloud login the VM shouldn't hold.
# zona/env-override keeps mise from installing them again.
for tool in rust gcloud; do
  sudo -iu "$user" mise uninstall --all "$tool" >/dev/null 2>&1 || true
done

echo "==> toolchains: $TOOLS"
sudo -iu "$user" bash -lc "cd $(printf %q "$repo") && mise trust --yes mise.toml >/dev/null && mise install $TOOLS && mise reshim"
# The runtime's Claude annotators and sanitizers run `claude -p` outside the
# checkout, where mise.toml doesn't apply: make its pin the VM-wide default.
sudo -iu "$user" bash -lc "cd $(printf %q "$repo") && mise use --global --quiet claude@\$(mise current claude)"
echo "==> done"
