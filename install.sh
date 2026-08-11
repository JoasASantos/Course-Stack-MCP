#!/usr/bin/env bash
# CourseStack MCP — one-line installer
#   curl -fsSL https://raw.githubusercontent.com/JoasASantos/Course-Stack-MCP/main/install.sh | bash
set -euo pipefail

REPO="JoasASantos/Course-Stack-MCP"
BIN_NAME="coursestack-mcp"
INSTALL_DIR="${COURSESTACK_INSTALL_DIR:-$HOME/.local/bin}"

log() { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

detect_target() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os-$arch" in
    Darwin-arm64)  echo "aarch64-apple-darwin" ;;
    Darwin-x86_64) echo "x86_64-apple-darwin" ;;
    Linux-x86_64)  echo "x86_64-unknown-linux-gnu" ;;
    Linux-aarch64) echo "aarch64-unknown-linux-gnu" ;;
    *) die "unsupported platform: $os $arch (build from source with 'cargo install --git https://github.com/$REPO')" ;;
  esac
}

install_from_release() {
  local target="$1" version="$2"
  local archive="$BIN_NAME-$target.tar.gz"
  local url="https://github.com/$REPO/releases/download/$version/$archive"
  local tmp
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT

  log "downloading $url"
  if ! curl -fsSL "$url" -o "$tmp/$archive"; then
    return 1
  fi
  tar -xzf "$tmp/$archive" -C "$tmp"
  mkdir -p "$INSTALL_DIR"
  install -m 755 "$tmp/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
  return 0
}

build_from_source() {
  command -v cargo >/dev/null 2>&1 || die "no prebuilt binary available and 'cargo' is not installed. Install Rust: https://rustup.rs"
  log "no prebuilt release found, building from source with cargo"
  local tmp
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  git clone --depth 1 "https://github.com/$REPO.git" "$tmp/src" 2>/dev/null \
    || die "git clone failed; is git installed?"
  (cd "$tmp/src" && cargo build --release)
  mkdir -p "$INSTALL_DIR"
  install -m 755 "$tmp/src/target/release/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
}

main() {
  local target version
  target=$(detect_target)
  version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
    | grep -m1 '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/' || true)

  if [ -z "${version:-}" ] || ! install_from_release "$target" "$version"; then
    build_from_source
  fi

  log "installed to $INSTALL_DIR/$BIN_NAME"
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) log "add $INSTALL_DIR to PATH: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
  esac

  echo
  log "next steps:"
  echo "  export COURSESTACK_API_KEY=sk_your_key"
  echo "  claude mcp add coursestack --scope user --env COURSESTACK_API_KEY=\$COURSESTACK_API_KEY -- $INSTALL_DIR/$BIN_NAME"
  echo "  $INSTALL_DIR/$BIN_NAME doctor    # verify credentials"
}

main "$@"
