#!/usr/bin/env bash
# Abstract CLI installer — macOS & Linux.
#
# Usage:
#   curl -fsSL https://cersei.pacifio.dev/install-abstract.sh | bash
#
# What it does:
#   1. Detects OS + architecture (macOS / Linux on x86_64 / arm64).
#   2. Installs build prerequisites (git, curl, a C toolchain) if missing.
#      - macOS: Xcode Command Line Tools.
#      - Debian/Ubuntu: apt install build-essential pkg-config libssl-dev.
#      - Fedora/RHEL:   dnf install gcc gcc-c++ make openssl-devel pkgconfig.
#      - Arch:          pacman -S base-devel openssl pkgconf.
#      - Alpine:        apk add build-base openssl-dev pkgconfig.
#   3. Installs the Rust toolchain via rustup (if cargo is not already on PATH).
#   4. Clones https://github.com/pacifio/cersei to $HOME/.abstract/src
#      (or pulls latest if it already exists).
#   5. Runs: cargo install --path crates/abstract-cli --bin abstract --locked
#      which places the binary at ~/.cargo/bin/abstract.
#   6. Prints a PATH hint if ~/.cargo/bin is not already on PATH.
#
# Environment overrides:
#   ABSTRACT_REF=<branch|tag|sha>   Checkout a specific ref (default: main).
#   ABSTRACT_SRC=<dir>              Override source clone location.
#   ABSTRACT_REPO=<git-url>         Override the source repo URL.
#   ABSTRACT_SKIP_DEPS=1            Don't try to install system packages.
#   ABSTRACT_UPGRADE=1              Force re-clone / reset local source tree.
#   NO_COLOR=1                      Disable colored output.
#
# The script is idempotent: running it twice upgrades the existing install.

set -euo pipefail

# ─── Configuration ─────────────────────────────────────────────────────────

ABSTRACT_REPO="${ABSTRACT_REPO:-https://github.com/pacifio/cersei.git}"
ABSTRACT_REF="${ABSTRACT_REF:-main}"
ABSTRACT_SRC="${ABSTRACT_SRC:-$HOME/.abstract/src}"
ABSTRACT_SKIP_DEPS="${ABSTRACT_SKIP_DEPS:-0}"
ABSTRACT_UPGRADE="${ABSTRACT_UPGRADE:-0}"

CARGO_BIN="$HOME/.cargo/bin"

# ─── Styling ───────────────────────────────────────────────────────────────

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  BOLD=$'\033[1m'
  DIM=$'\033[2m'
  RED=$'\033[31m'
  GREEN=$'\033[32m'
  YELLOW=$'\033[33m'
  CYAN=$'\033[36m'
  RESET=$'\033[0m'
else
  BOLD="" DIM="" RED="" GREEN="" YELLOW="" CYAN="" RESET=""
fi

info()    { printf '%s==>%s %s\n'   "$CYAN"   "$RESET" "$*"; }
success() { printf '%s✓%s %s\n'     "$GREEN"  "$RESET" "$*"; }
warn()    { printf '%s⚠%s %s\n'     "$YELLOW" "$RESET" "$*" >&2; }
die()     { printf '%s✗%s %s\n'     "$RED"    "$RESET" "$*" >&2; exit 1; }

# ─── OS / architecture detection ───────────────────────────────────────────

detect_os() {
  local uname_s
  uname_s="$(uname -s)"
  case "$uname_s" in
    Darwin) echo "macos" ;;
    Linux)  echo "linux" ;;
    *)      die "Unsupported OS: $uname_s (only macOS and Linux are supported)" ;;
  esac
}

detect_arch() {
  local uname_m
  uname_m="$(uname -m)"
  case "$uname_m" in
    x86_64|amd64)  echo "x86_64" ;;
    arm64|aarch64) echo "arm64" ;;
    *)             echo "$uname_m" ;; # still attempt — rustc may support it
  esac
}

# Try to identify the Linux distro family. Outputs one of:
# debian | fedora | arch | alpine | unknown
detect_linux_family() {
  if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    case "${ID:-}:${ID_LIKE:-}" in
      *debian*|*ubuntu*) echo "debian"; return ;;
      *fedora*|*rhel*|*centos*) echo "fedora"; return ;;
      *arch*) echo "arch"; return ;;
      *alpine*) echo "alpine"; return ;;
    esac
  fi
  echo "unknown"
}

# ─── Prerequisite install ──────────────────────────────────────────────────

have() { command -v "$1" >/dev/null 2>&1; }

ensure_sudo() {
  if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
  elif have sudo; then
    SUDO="sudo"
  else
    warn "No sudo available — you may need to install system packages manually."
    SUDO=""
  fi
}

install_macos_prereqs() {
  if ! xcode-select -p >/dev/null 2>&1; then
    info "Triggering Xcode Command Line Tools install (a dialog will open)..."
    xcode-select --install || true
    warn "Rerun this installer once the Command Line Tools install completes."
    exit 1
  fi
}

install_linux_prereqs() {
  local family
  family="$(detect_linux_family)"
  ensure_sudo

  case "$family" in
    debian)
      info "Installing build prerequisites via apt..."
      $SUDO apt-get update -y
      $SUDO apt-get install -y --no-install-recommends \
        git curl ca-certificates build-essential pkg-config libssl-dev
      ;;
    fedora)
      info "Installing build prerequisites via dnf..."
      $SUDO dnf install -y \
        git curl ca-certificates gcc gcc-c++ make openssl-devel pkgconfig
      ;;
    arch)
      info "Installing build prerequisites via pacman..."
      $SUDO pacman -Sy --noconfirm --needed \
        git curl ca-certificates base-devel openssl pkgconf
      ;;
    alpine)
      info "Installing build prerequisites via apk..."
      $SUDO apk add --no-cache \
        git curl ca-certificates build-base openssl-dev pkgconfig
      ;;
    *)
      warn "Unknown Linux distribution — skipping package install."
      warn "Make sure you have: git, curl, a C/C++ toolchain, pkg-config, openssl dev headers."
      ;;
  esac
}

ensure_prereqs() {
  local os="$1"
  if [ "$ABSTRACT_SKIP_DEPS" = "1" ]; then
    info "ABSTRACT_SKIP_DEPS=1 — skipping system package install."
    return
  fi
  case "$os" in
    macos) install_macos_prereqs ;;
    linux) install_linux_prereqs ;;
  esac
}

# ─── Rust toolchain ────────────────────────────────────────────────────────

ensure_rust() {
  if have cargo && have rustc; then
    success "Rust toolchain already installed ($(rustc --version))"
    return
  fi

  # Also check ~/.cargo/bin — rustup may have installed but not be on PATH yet.
  if [ -x "$CARGO_BIN/cargo" ] && [ -x "$CARGO_BIN/rustc" ]; then
    export PATH="$CARGO_BIN:$PATH"
    success "Using existing Rust toolchain at $CARGO_BIN"
    return
  fi

  info "Installing Rust toolchain via rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path

  # Bring cargo into this shell's PATH.
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  export PATH="$CARGO_BIN:$PATH"

  have cargo || die "Rust install succeeded but cargo is not on PATH — try opening a new shell and rerunning."
  success "Installed $(rustc --version)"
}

# ─── Source checkout ───────────────────────────────────────────────────────

fetch_source() {
  mkdir -p "$(dirname "$ABSTRACT_SRC")"

  if [ -d "$ABSTRACT_SRC/.git" ] && [ "$ABSTRACT_UPGRADE" != "1" ]; then
    info "Updating existing checkout at $ABSTRACT_SRC"
    git -C "$ABSTRACT_SRC" fetch --tags --prune origin
    git -C "$ABSTRACT_SRC" checkout "$ABSTRACT_REF"
    # Fast-forward to latest on this ref; tolerate detached-HEAD tags/shas.
    git -C "$ABSTRACT_SRC" pull --ff-only 2>/dev/null || true
  else
    if [ -e "$ABSTRACT_SRC" ]; then
      info "Removing stale source at $ABSTRACT_SRC"
      rm -rf "$ABSTRACT_SRC"
    fi
    info "Cloning $ABSTRACT_REPO (ref: $ABSTRACT_REF) into $ABSTRACT_SRC"
    git clone --depth 50 "$ABSTRACT_REPO" "$ABSTRACT_SRC"
    git -C "$ABSTRACT_SRC" checkout "$ABSTRACT_REF"
  fi
}

# ─── Build & install ───────────────────────────────────────────────────────

build_and_install() {
  info "Building abstract (release mode — this takes a few minutes on a cold cache)..."
  (
    cd "$ABSTRACT_SRC"
    cargo install \
      --path crates/abstract-cli \
      --bin abstract \
      --locked \
      --force
  )
  success "Installed $(abstract --version 2>/dev/null || echo 'abstract') to $CARGO_BIN/abstract"
}

# ─── PATH hint ─────────────────────────────────────────────────────────────

path_hint() {
  case ":$PATH:" in
    *":$CARGO_BIN:"*) return ;;
  esac

  local rc=""
  if [ -n "${ZSH_VERSION:-}" ] || [ -n "${ZDOTDIR:-}" ] || [ "${SHELL:-}" = "/bin/zsh" ] || [ "${SHELL:-}" = "/usr/bin/zsh" ]; then
    rc="$HOME/.zshrc"
  elif [ -n "${BASH_VERSION:-}" ] || [ "${SHELL:-}" = "/bin/bash" ] || [ "${SHELL:-}" = "/usr/bin/bash" ]; then
    rc="$HOME/.bashrc"
  fi

  printf '\n'
  warn "$CARGO_BIN is not on your PATH."
  printf '   Add this line to %s:\n\n' "${rc:-your shell rc file}"
  printf '       %sexport PATH="$HOME/.cargo/bin:$PATH"%s\n\n' "$BOLD" "$RESET"
  printf '   Then reload your shell: %sexec $SHELL -l%s\n' "$BOLD" "$RESET"
}

# ─── Banner ────────────────────────────────────────────────────────────────

banner() {
  printf '%s\n' "${BOLD}${CYAN}"
  printf '   abstract — installer\n'
  printf '%s   %shttps://github.com/pacifio/cersei%s\n\n' "$RESET" "$DIM" "$RESET"
}

# ─── Main ──────────────────────────────────────────────────────────────────

main() {
  banner

  local os arch
  os="$(detect_os)"
  arch="$(detect_arch)"
  info "Detected: ${BOLD}${os}${RESET} on ${BOLD}${arch}${RESET}"

  ensure_prereqs "$os"

  have git  || die "git is required but not installed."
  have curl || die "curl is required but not installed."

  ensure_rust
  fetch_source
  build_and_install

  printf '\n%s%sAbstract is installed.%s\n' "$GREEN" "$BOLD" "$RESET"
  printf '   Try it out:     %sabstract --help%s\n' "$BOLD" "$RESET"
  printf '   Authenticate:   %sabstract login%s\n'  "$BOLD" "$RESET"
  printf '   Project setup:  %sabstract init%s\n'   "$BOLD" "$RESET"

  path_hint
}

main "$@"
