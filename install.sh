#!/usr/bin/env bash
set -euo pipefail

# Nenjo install script
#
# Usage:
#   curl -fsSL https://nenjo.ai/install | bash
#   curl -fsSL https://nenjo.ai/install | bash -s -- --version v0.1.1

REPO="nenjo-ai/nenjo"
BINARY_NAMES=("nenjo" "nenpm" "nenjoup")
INSTALL_DIR="${NENJO_INSTALL_DIR:-$HOME/.nenjo/bin}"
TMP_DIR=""

cleanup() {
  if [[ -n "$TMP_DIR" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

# Parse arguments
VERSION=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --help|-h)
      echo "Usage: install.sh [--version <tag>]"
      echo ""
      echo "Install the Nenjo command-line tools."
      echo ""
      echo "Options:"
      echo "  --version <tag>   Install a specific version (e.g. v0.1.1). Default: latest."
      echo ""
      echo "Environment:"
      echo "  NENJO_INSTALL_DIR   Override install directory (default: ~/.nenjo/bin)"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# Detect OS and architecture
detect_platform() {
  local os arch

  case "$(uname -s)" in
    Linux*)  os="unknown-linux-gnu" ;;
    Darwin*) os="apple-darwin" ;;
    *)       echo "Error: unsupported OS '$(uname -s)'"; exit 1 ;;
  esac

  case "$(uname -m)" in
    x86_64|amd64)  arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *)             echo "Error: unsupported architecture '$(uname -m)'"; exit 1 ;;
  esac

  local target="${arch}-${os}"
  case "$target" in
    x86_64-unknown-linux-gnu|aarch64-apple-darwin) echo "$target" ;;
    *)
      echo "Error: no published Nenjo binary bundle for ${target}" >&2
      exit 1
      ;;
  esac
}

# Resolve version tag
resolve_version() {
  if [[ -n "$VERSION" ]]; then
    echo "$VERSION"
    return
  fi

  local latest
  latest=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')

  if [[ -z "$latest" ]]; then
    echo "Error: could not determine latest release" >&2
    exit 1
  fi

  echo "$latest"
}

# Add INSTALL_DIR to PATH in the user's shell profile
ensure_path() {
  if [[ ":$PATH:" == *":${INSTALL_DIR}:"* ]]; then
    return
  fi

  local line="export PATH=\"${INSTALL_DIR}:\$PATH\""
  local profile=""

  # Find the right shell profile
  case "${SHELL:-}" in
    */zsh)  profile="$HOME/.zshrc" ;;
    */bash)
      if [[ -f "$HOME/.bash_profile" ]]; then
        profile="$HOME/.bash_profile"
      else
        profile="$HOME/.bashrc"
      fi
      ;;
    */fish)
      # fish uses a different syntax
      local fish_line="fish_add_path ${INSTALL_DIR}"
      local fish_config="$HOME/.config/fish/config.fish"
      if [[ -f "$fish_config" ]] && grep -qF "$INSTALL_DIR" "$fish_config" 2>/dev/null; then
        return
      fi
      mkdir -p "$(dirname "$fish_config")"
      echo "$fish_line" >> "$fish_config"
      echo "Added ${INSTALL_DIR} to PATH in ${fish_config}"
      return
      ;;
    *)
      if [[ -f "$HOME/.profile" ]]; then
        profile="$HOME/.profile"
      fi
      ;;
  esac

  if [[ -z "$profile" ]]; then
    echo ""
    echo "Add ${INSTALL_DIR} to your PATH manually:"
    echo ""
    echo "  $line"
    return
  fi

  # Don't duplicate if already present
  if grep -qF "$INSTALL_DIR" "$profile" 2>/dev/null; then
    return
  fi

  echo "" >> "$profile"
  echo "# Nenjo CLI" >> "$profile"
  echo "$line" >> "$profile"
  echo "Added ${INSTALL_DIR} to PATH in ${profile}"
  echo "Run 'source ${profile}' or restart your shell to use nenjo."
}

main() {
  local platform version artifact_name checksum_name download_url checksum_url

  platform=$(detect_platform)
  version=$(resolve_version)

  echo "Installing Nenjo ${version} for ${platform}..."

  artifact_name="nenjo-${platform}.tar.gz"
  checksum_name="${artifact_name}.sha256"
  download_url="https://github.com/${REPO}/releases/download/${version}/${artifact_name}"
  checksum_url="${download_url}.sha256"

  # Download to temp directory
  TMP_DIR=$(mktemp -d)

  echo "Downloading ${download_url}..."
  if ! curl -fSL -o "${TMP_DIR}/${artifact_name}" "$download_url"; then
    echo ""
    echo "Error: failed to download ${artifact_name}"
    echo ""
    echo "Available at: https://github.com/${REPO}/releases/tag/${version}"
    echo ""
    echo "Make sure the release exists and has a binary for your platform (${platform})."
    exit 1
  fi
  echo "Downloading ${checksum_url}..."
  if ! curl -fSL -o "${TMP_DIR}/${checksum_name}" "$checksum_url"; then
    echo ""
    echo "Error: failed to download ${checksum_name}"
    exit 1
  fi

  verify_checksum "${artifact_name}" "${checksum_name}"

  # Extract and install
  tar -xzf "${TMP_DIR}/${artifact_name}" -C "${TMP_DIR}"
  mkdir -p "$INSTALL_DIR"

  for binary in "${BINARY_NAMES[@]}"; do
    if [[ ! -f "${TMP_DIR}/${binary}" ]]; then
      echo "Error: release bundle is missing ${binary}" >&2
      exit 1
    fi
    chmod +x "${TMP_DIR}/${binary}"
    mv "${TMP_DIR}/${binary}" "${INSTALL_DIR}/${binary}"
    echo "Installed ${binary} to ${INSTALL_DIR}/${binary}"
  done

  # Add to PATH
  ensure_path

  echo ""
  export PATH="${INSTALL_DIR}:$PATH"
  for binary in "${BINARY_NAMES[@]}"; do
    "${INSTALL_DIR}/${binary}" --version 2>/dev/null || true
  done
}

verify_checksum() {
  local artifact_name="$1"
  local checksum_name="$2"

  if command -v shasum >/dev/null 2>&1; then
    (cd "$TMP_DIR" && shasum -a 256 -c "$checksum_name")
    return
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$TMP_DIR" && sha256sum -c "$checksum_name")
    return
  fi

  echo "Error: cannot verify ${artifact_name}; install shasum or sha256sum" >&2
  exit 1
}

main
