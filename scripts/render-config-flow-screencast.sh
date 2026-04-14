#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

prepend_path_if_dir() {
  if [ -d "$1" ]; then
    case ":$PATH:" in
      *":$1:"*) ;;
      *) PATH="$1:$PATH" ;;
    esac
  fi
}

prepend_path_if_dir /opt/homebrew/bin
prepend_path_if_dir /usr/local/bin
export PATH

OUTPUT_DIR="$REPO_ROOT/target/screencasts"
OUTPUT_BASENAME="pw-env-config-init-migrate-auto-load"
SKIP_BUILD=0
KEEP_WORKDIR=0
UPDATE_DOCS=0
WORKDIR=""

usage() {
  cat <<'EOF'
Usage: render-config-flow-screencast.sh [options]

Build the release pw-env binary, prepare an isolated GPG-backed demo workspace,
and render a VHS screencast that shows configuration, eval-based initialization,
migration, and automatic loading.

Options:
  --output-dir <dir>  Directory for rendered assets. Defaults to target/screencasts.
  --skip-build        Reuse the existing target/release/pw-env binary.
  --keep-workdir      Keep the generated tape and demo workspace after rendering.
  --update-docs       Copy the rendered GIF to docs/pw-env-config-init-migrate-auto-load.gif.
  -h, --help          Show this help text.
EOF
}

fail() {
  echo "Error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --output-dir)
      [ $# -ge 2 ] || fail "--output-dir requires a value"
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --keep-workdir)
      KEEP_WORKDIR=1
      shift
      ;;
    --update-docs)
      UPDATE_DOCS=1
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

need_cmd bash
need_cmd cargo
need_cmd gpg
need_cmd ffmpeg
need_cmd ttyd
need_cmd vhs

PW_ENV_BIN="$REPO_ROOT/target/release/pw-env"
WORKDIR="/tmp/${OUTPUT_BASENAME}-work"
HOME_DIR="/tmp/pw-env-demo-home"
XDG_CONFIG_HOME="$HOME_DIR/.config"
XDG_STATE_HOME="$HOME_DIR/.local/state"
GNUPGHOME="$HOME_DIR/.gnupg"
PROJECT_DIR="$HOME_DIR/demo-app"
CONFIG_PATH="$XDG_CONFIG_HOME/pw-env/config.toml"
GPG_BATCH_PATH="$WORKDIR/demo-gpg.batch"
TAPE_PATH="$WORKDIR/${OUTPUT_BASENAME}.tape"
GIF_PATH="$OUTPUT_DIR/${OUTPUT_BASENAME}.gif"
MP4_PATH="$OUTPUT_DIR/${OUTPUT_BASENAME}.mp4"
DOCS_GIF_PATH="$REPO_ROOT/docs/${OUTPUT_BASENAME}.gif"

cleanup() {
  if [ "$KEEP_WORKDIR" -eq 0 ] && [ -n "$WORKDIR" ] && [ -d "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
  fi
  if [ "$KEEP_WORKDIR" -eq 0 ] && [ -n "$HOME_DIR" ] && [ -d "$HOME_DIR" ]; then
    rm -rf "$HOME_DIR"
  fi
}

trap cleanup EXIT

mkdir -p "$OUTPUT_DIR"
rm -rf "$WORKDIR" "$HOME_DIR"
mkdir -p "$WORKDIR" "$HOME_DIR" "$XDG_CONFIG_HOME/pw-env" "$XDG_STATE_HOME" "$GNUPGHOME" "$PROJECT_DIR/nested"
chmod 700 "$GNUPGHOME"
rm -f "$GIF_PATH" "$MP4_PATH"

if [ "$SKIP_BUILD" -eq 0 ]; then
  (
    cd "$REPO_ROOT"
    cargo build --release --locked
  )
fi

[ -x "$PW_ENV_BIN" ] || fail "expected release binary at $PW_ENV_BIN"

cat > "$CONFIG_PATH" <<'EOF'
[defaults]
backend = "gpg"

[defaults.gpg]
file_pattern = ".env.gpg"
recipient = "demo@example.com"

[updates]
enabled = false
EOF

cat > "$PROJECT_DIR/.env" <<'EOF'
DATABASE_URL=postgres://demo:correct-horse-battery-staple@db.internal:5432/app
API_KEY=demo-api-key-123456789

# pw-env:ignore
LOG_LEVEL=debug
EOF

cat > "$GPG_BATCH_PATH" <<'EOF'
%no-protection
Key-Type: RSA
Key-Length: 2048
Subkey-Type: RSA
Subkey-Length: 2048
Name-Real: pw-env Demo
Name-Email: demo@example.com
Expire-Date: 0
%commit
EOF

gpg --batch --homedir "$GNUPGHOME" --generate-key "$GPG_BATCH_PATH" >/dev/null 2>&1
XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" GNUPGHOME="$GNUPGHOME" HOME="$HOME_DIR" \
  "$PW_ENV_BIN" approvals approve-fetch "$PROJECT_DIR" --project-wide >/dev/null 2>&1

cat > "$TAPE_PATH" <<EOF
Output "$GIF_PATH"
Output "$MP4_PATH"

Require bash
Require gpg

Set Shell bash
Set FontSize 26
Set Width 1500
Set Height 900
Set Padding 20
Set TypingSpeed 35ms
Set PlaybackSpeed 0.75
Set CursorBlink false
Set WindowBar Colorful

Hide
Type "cd $REPO_ROOT"
Enter
Type "export PATH=$REPO_ROOT/target/release:\$PATH"
Enter
Type "export XDG_CONFIG_HOME=$XDG_CONFIG_HOME"
Enter
Type "export XDG_STATE_HOME=$XDG_STATE_HOME"
Enter
Type "export GNUPGHOME=$GNUPGHOME"
Enter
Type "export HOME=$HOME_DIR"
Enter
Type "export PROJECT_DIR=~/demo-app"
Enter
Type "clear"
Enter
Show

Type "pw-env --version"
Enter
Sleep 3s

Type "cat ~/.config/pw-env/config.toml"
Enter
Sleep 4s

Type "cd ~/demo-app"
Enter
Sleep 1500ms

Type "cat .env"
Enter
Sleep 4s

Type "pw-env migrate ."
Hide
Enter
Sleep 2s
Enter
Sleep 3s
Type "clear"
Enter
Sleep 1500ms
Show
Sleep 1000ms

Type "cat .env"
Enter
Sleep 4s

Type "ls -1a"
Enter
Sleep 3s

Type "cd .."
Enter
Sleep 1500ms

Type \`eval "\$(pw-env init bash)"\`
Enter
Sleep 2500ms

Type "cd ~/demo-app"
Enter
Sleep 3500ms

Type "printenv DATABASE_URL"
Enter
Sleep 3500ms

Type "printenv API_KEY"
Enter
Sleep 3500ms

Type "cd nested"
Enter
Sleep 2000ms

Type "printenv API_KEY"
Enter
Sleep 3500ms

Type "cd ../.."
Enter
Sleep 2000ms

Type "printenv API_KEY || echo API_KEY cleared outside the project"
Enter
Sleep 3500ms
EOF

vhs "$TAPE_PATH"

if [ "$UPDATE_DOCS" -ne 0 ]; then
  if [ "$GIF_PATH" != "$DOCS_GIF_PATH" ]; then
    cp "$GIF_PATH" "$DOCS_GIF_PATH"
  fi
  echo "Updated docs GIF: $DOCS_GIF_PATH"
fi

echo "Rendered GIF: $GIF_PATH"
echo "Rendered MP4: $MP4_PATH"

if [ "$KEEP_WORKDIR" -ne 0 ]; then
  echo "Tape file: $TAPE_PATH"
fi
