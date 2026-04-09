#!/usr/bin/env bash
set -Eeuo pipefail

REPO_URL="${EMMC_LAB_REPO_URL:-https://github.com/nbeathoven/emmc-lab.git}"
INSTALL_PREFIX="${EMMC_LAB_PREFIX:-/usr/local}"
INSTALL_SRC_DIR="${EMMC_LAB_SRC_DIR:-$HOME/.local/src/emmc-lab}"
RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
INSTALL_OPTIONAL="${EMMC_LAB_INSTALL_OPTIONAL:-1}"
APT_UPDATED=0
SUDO=""

if [[ "$(id -u)" -ne 0 ]]; then
  SUDO="sudo"
fi

log() {
  printf '[emmc-lab-install] %s\n' "$*"
}

apt_update_once() {
  if [[ "$APT_UPDATED" -eq 0 ]]; then
    log "Refreshing apt package index"
    $SUDO apt-get update
    APT_UPDATED=1
  fi
}

package_installed() {
  dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q "install ok installed"
}

ensure_package() {
  local pkg="$1"
  if package_installed "$pkg"; then
    log "Package already installed: $pkg"
    return
  fi
  apt_update_once
  log "Installing missing package: $pkg"
  $SUDO apt-get install -y "$pkg"
}

ensure_command() {
  local cmd="$1"
  local pkg="$2"
  if command -v "$cmd" >/dev/null 2>&1; then
    log "Command already available: $cmd"
  else
    ensure_package "$pkg"
  fi
}

ensure_build_packages() {
  ensure_package ca-certificates
  ensure_package curl
  ensure_package git
  ensure_package build-essential
  ensure_package pkg-config
}

ensure_optional_packages() {
  if [[ "$INSTALL_OPTIONAL" != "1" ]]; then
    log "Skipping optional packages because EMMC_LAB_INSTALL_OPTIONAL=$INSTALL_OPTIONAL"
    return
  fi
  ensure_package mmc-utils
  ensure_package fio
}

ensure_rust_toolchain() {
  export PATH="$CARGO_HOME/bin:$PATH"
  if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
    log "Rust toolchain already available"
    return
  fi

  log "Installing Rust toolchain with rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$CARGO_HOME/bin:$PATH"
  command -v cargo >/dev/null 2>&1 || {
    log "cargo was not found after rustup installation"
    exit 1
  }
}

resolve_repo_dir() {
  local script_dir=""
  if [[ -n "${BASH_SOURCE[0]:-}" ]]; then
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    if [[ -f "$script_dir/../Cargo.toml" ]]; then
      cd "$script_dir/.." && pwd
      return
    fi
  fi

  mkdir -p "$(dirname "$INSTALL_SRC_DIR")"
  if [[ -d "$INSTALL_SRC_DIR/.git" ]]; then
    log "Updating existing source checkout in $INSTALL_SRC_DIR"
    git -C "$INSTALL_SRC_DIR" fetch --tags --prune origin
    git -C "$INSTALL_SRC_DIR" pull --ff-only origin main || true
  else
    log "Cloning source from $REPO_URL to $INSTALL_SRC_DIR"
    rm -rf "$INSTALL_SRC_DIR"
    git clone "$REPO_URL" "$INSTALL_SRC_DIR"
  fi
  cd "$INSTALL_SRC_DIR" && pwd
}

build_binary() {
  local repo_dir="$1"
  export PATH="$CARGO_HOME/bin:$PATH"
  log "Building emmc-lab in release mode"
  cargo -C "$repo_dir" build --release
}

install_binary() {
  local repo_dir="$1"
  local source_bin="$repo_dir/target/release/emmc-lab"
  if [[ ! -x "$source_bin" ]]; then
    log "Expected build output missing: $source_bin"
    exit 1
  fi
  log "Installing binary to $INSTALL_PREFIX/bin/emmc-lab"
  $SUDO install -d "$INSTALL_PREFIX/bin"
  $SUDO install -m 0755 "$source_bin" "$INSTALL_PREFIX/bin/emmc-lab"
}

print_post_install() {
  cat <<EOF

emmc-lab installation complete.

Run the application with:
  emmc-lab

Useful first commands:
  emmc-lab doctor
  emmc-lab wizard
  emmc-lab health --device /dev/<device>

Self-deploy command from GitHub:
  curl -fsSL https://raw.githubusercontent.com/nbeathoven/emmc-lab/main/rpi/install.sh | bash
EOF
}

main() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    log "This installer is intended for Linux on Debian-family distributions, including Raspberry Pi OS"
    exit 1
  fi

  ensure_build_packages
  ensure_optional_packages
  ensure_rust_toolchain
  local repo_dir
  repo_dir="$(resolve_repo_dir)"
  build_binary "$repo_dir"
  install_binary "$repo_dir"
  print_post_install
}

main "$@"
