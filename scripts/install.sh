#!/usr/bin/env bash
set -euo pipefail

OWNER="m42e"
REPO="pw-env"
BINARY_NAME="pw-env"
VERSION="latest"
DRY_RUN=0
ARCHIVE_FORMAT=""
ARCHIVE_BINARY_NAME="$BINARY_NAME"

usage() {
  cat <<'EOF'
Usage: install.sh [--version <version>] [--dir <install-dir>] [--dry-run]

Downloads the matching pw-env release archive for the current platform and
installs the binary.

Options:
  --version <version>  Release version to install (e.g. 0.1.0 or v0.1.0).
                       Defaults to the latest GitHub release.
  --dir <install-dir>  Destination directory for the binary.
                       Defaults to /usr/local/bin when writable, otherwise
                       ~/.local/bin.
  --dry-run            Print the resolved download URL and exit.
  -h, --help           Show this help text.

Environment:
  INSTALL_DIR          Overrides the destination directory.
  GITHUB_TOKEN         Optional token for GitHub API requests.
EOF
}

fail() {
  echo "Error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

download_text() {
  url="$1"

  if command -v curl >/dev/null 2>&1; then
    if [ -n "${GITHUB_TOKEN:-}" ]; then
      curl -fsSL \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        -H "Authorization: Bearer ${GITHUB_TOKEN}" \
        "$url"
    else
      curl -fsSL \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        "$url"
    fi
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    if [ -n "${GITHUB_TOKEN:-}" ]; then
      wget -qO- \
        --header="Accept: application/vnd.github+json" \
        --header="X-GitHub-Api-Version: 2022-11-28" \
        --header="Authorization: Bearer ${GITHUB_TOKEN}" \
        "$url"
    else
      wget -qO- \
        --header="Accept: application/vnd.github+json" \
        --header="X-GitHub-Api-Version: 2022-11-28" \
        "$url"
    fi
    return
  fi

  fail "either curl or wget is required"
}

download_file() {
  url="$1"
  destination="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$destination"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -qO "$destination" "$url"
    return
  fi

  fail "either curl or wget is required"
}

extract_archive() {
  archive_path="$1"
  destination="$2"

  case "$ARCHIVE_FORMAT" in
    tar.gz)
      tar -xzf "$archive_path" -C "$destination"
      ;;
    zip)
      if command -v unzip >/dev/null 2>&1; then
        unzip -q "$archive_path" -d "$destination"
      elif command -v bsdtar >/dev/null 2>&1; then
        bsdtar -xf "$archive_path" -C "$destination"
      else
        fail "zip archive extraction requires unzip or bsdtar"
      fi
      ;;
    *)
      fail "unsupported archive format: $ARCHIVE_FORMAT"
      ;;
  esac
}

default_install_dir() {
  if [ -n "${INSTALL_DIR:-}" ]; then
    printf '%s\n' "$INSTALL_DIR"
    return
  fi

  if [ -w /usr/local/bin ] || { [ ! -e /usr/local/bin ] && [ -d /usr/local ] && [ -w /usr/local ]; }; then
    printf '%s\n' "/usr/local/bin"
    return
  fi

  [ -n "${HOME:-}" ] || fail "HOME is not set and no install directory was provided"
  printf '%s\n' "$HOME/.local/bin"
}

normalize_tag() {
  input="$1"
  case "$input" in
    latest)
      printf '%s\n' "latest"
      ;;
    v*)
      printf '%s\n' "$input"
      ;;
    *)
      printf 'v%s\n' "$input"
      ;;
  esac
}

resolve_latest_tag() {
  response=$(download_text "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest")
  tag=$(printf '%s\n' "$response" | awk -F '"' '/"tag_name"[[:space:]]*:/ { print $4; exit }')
  [ -n "$tag" ] || fail "unable to resolve the latest release tag"
  printf '%s\n' "$tag"
}

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)

  case "$os" in
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
      case "$arch" in
        x86_64|amd64)
          printf '%s\t%s\t%s\n' "x86_64-pc-windows-msvc" "zip" "${BINARY_NAME}.exe"
          ;;
        *)
          fail "unsupported Windows architecture: $arch"
          ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        arm64|aarch64)
          printf '%s\t%s\t%s\n' "aarch64-apple-darwin" "tar.gz" "$BINARY_NAME"
          ;;
        x86_64|amd64)
          printf '%s\t%s\t%s\n' "x86_64-apple-darwin" "tar.gz" "$BINARY_NAME"
          ;;
        *)
          fail "unsupported macOS architecture: $arch"
          ;;
      esac
      ;;
    Linux)
      case "$arch" in
        arm64|aarch64)
          printf '%s\t%s\t%s\n' "aarch64-unknown-linux-gnu" "tar.gz" "$BINARY_NAME"
          ;;
        x86_64|amd64)
          printf '%s\t%s\t%s\n' "x86_64-unknown-linux-gnu" "tar.gz" "$BINARY_NAME"
          ;;
        *)
          fail "unsupported Linux architecture: $arch"
          ;;
      esac
      ;;
    *)
      fail "unsupported operating system: $os"
      ;;
  esac
}

print_post_install_note() {
  install_dir="$1"

  case ":$PATH:" in
    *":$install_dir:"*)
      ;;
    *)
      echo ""
      echo "Add $install_dir to PATH if it is not already available in your shell session."
      ;;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || fail "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --dir)
      [ $# -ge 2 ] || fail "--dir requires a value"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

need_cmd uname
need_cmd mktemp

IFS=$'\t' read -r target ARCHIVE_FORMAT ARCHIVE_BINARY_NAME <<EOF
$(detect_target)
EOF
tag=$(normalize_tag "$VERSION")
if [ "$tag" = "latest" ]; then
  tag=$(resolve_latest_tag)
fi

archive_name="${BINARY_NAME}-${tag}-${target}.${ARCHIVE_FORMAT}"
download_url="https://github.com/${OWNER}/${REPO}/releases/download/${tag}/${archive_name}"
install_dir=$(default_install_dir)
install_path="${install_dir}/${ARCHIVE_BINARY_NAME}"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "tag=$tag"
  echo "target=$target"
  echo "archive=$archive_name"
  echo "url=$download_url"
  echo "install_dir=$install_dir"
  exit 0
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$install_dir"
archive_path="${tmpdir}/${archive_name}"

echo "Downloading ${archive_name}..."
download_file "$download_url" "$archive_path"

echo "Installing ${BINARY_NAME} to ${install_path}..."
extract_archive "$archive_path" "$tmpdir"
[ -f "${tmpdir}/${ARCHIVE_BINARY_NAME}" ] || fail "archive did not contain ${ARCHIVE_BINARY_NAME}"

if command -v install >/dev/null 2>&1; then
  install -m 0755 "${tmpdir}/${ARCHIVE_BINARY_NAME}" "$install_path"
else
  cp "${tmpdir}/${ARCHIVE_BINARY_NAME}" "$install_path"
  chmod 0755 "$install_path"
fi

echo "Installed ${BINARY_NAME} ${tag} to ${install_path}"
print_post_install_note "$install_dir"
