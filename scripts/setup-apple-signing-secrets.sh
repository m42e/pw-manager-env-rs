#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: setup-apple-signing-secrets.sh --cert <certificate.p12> --cert-password <password> \
    --identity <signing identity> [options]

Prepare the GitHub Actions secrets used for optional macOS signing and notarization.

Required:
  --cert <path>               Path to the exported Developer ID Application .p12 file.
  --cert-password <password>  Password used when exporting the .p12 file.
  --identity <identity>       Full codesigning identity, for example:
                              Developer ID Application: Example Corp (TEAMID1234)

Optional:
  --apple-id <email>          Apple ID used for notarization.
  --app-password <password>   App-specific password for notarization.
  --team-id <team-id>         Apple Developer Team ID for notarization.
  --repo <owner/repo>         GitHub repository for `gh secret set`.
  --set-gh-secrets            Upload secrets directly with GitHub CLI.
  --env-file <path>           Write KEY=VALUE lines to a file instead of stdout.
  -h, --help                  Show this help text.

Behavior:
  - Always prepares the signing secrets.
  - Only prepares notarization secrets when --apple-id, --app-password, and
    --team-id are all provided.
  - With --set-gh-secrets, uploads the prepared values as repository secrets.
  - Without --set-gh-secrets, prints KEY=VALUE lines so you can inspect them.
EOF
}

fail() {
  echo "Error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

base64_no_wrap() {
  if base64 < /dev/null | tr -d '\n' >/dev/null 2>&1; then
    base64 | tr -d '\n'
    return
  fi

  fail "unable to encode base64 on this system"
}

write_output() {
  if [ -n "$ENV_FILE" ]; then
    printf '%s\n' "$1" >> "$ENV_FILE"
  else
    printf '%s\n' "$1"
  fi
}

upload_secret() {
  local name="$1"
  local value="$2"

  if [ -n "$REPO" ]; then
    gh secret set "$name" --repo "$REPO" --body "$value" >/dev/null
  else
    gh secret set "$name" --body "$value" >/dev/null
  fi
}

CERT_PATH=""
CERT_PASSWORD=""
SIGNING_IDENTITY=""
APPLE_ID=""
APP_PASSWORD=""
TEAM_ID=""
REPO=""
SET_GH_SECRETS=0
ENV_FILE=""

while [ $# -gt 0 ]; do
  case "$1" in
    --cert)
      [ $# -ge 2 ] || fail "--cert requires a value"
      CERT_PATH="$2"
      shift 2
      ;;
    --cert-password)
      [ $# -ge 2 ] || fail "--cert-password requires a value"
      CERT_PASSWORD="$2"
      shift 2
      ;;
    --identity)
      [ $# -ge 2 ] || fail "--identity requires a value"
      SIGNING_IDENTITY="$2"
      shift 2
      ;;
    --apple-id)
      [ $# -ge 2 ] || fail "--apple-id requires a value"
      APPLE_ID="$2"
      shift 2
      ;;
    --app-password)
      [ $# -ge 2 ] || fail "--app-password requires a value"
      APP_PASSWORD="$2"
      shift 2
      ;;
    --team-id)
      [ $# -ge 2 ] || fail "--team-id requires a value"
      TEAM_ID="$2"
      shift 2
      ;;
    --repo)
      [ $# -ge 2 ] || fail "--repo requires a value"
      REPO="$2"
      shift 2
      ;;
    --set-gh-secrets)
      SET_GH_SECRETS=1
      shift
      ;;
    --env-file)
      [ $# -ge 2 ] || fail "--env-file requires a value"
      ENV_FILE="$2"
      shift 2
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

[ -n "$CERT_PATH" ] || fail "--cert is required"
[ -f "$CERT_PATH" ] || fail "certificate file not found: $CERT_PATH"
[ -n "$CERT_PASSWORD" ] || fail "--cert-password is required"
[ -n "$SIGNING_IDENTITY" ] || fail "--identity is required"

need_cmd base64

if [ "$SET_GH_SECRETS" -eq 1 ]; then
  need_cmd gh
fi

HAS_NOTARIZATION=0
if [ -n "$APPLE_ID" ] || [ -n "$APP_PASSWORD" ] || [ -n "$TEAM_ID" ]; then
  [ -n "$APPLE_ID" ] || fail "--apple-id is required when notarization values are provided"
  [ -n "$APP_PASSWORD" ] || fail "--app-password is required when notarization values are provided"
  [ -n "$TEAM_ID" ] || fail "--team-id is required when notarization values are provided"
  HAS_NOTARIZATION=1
fi

if [ -n "$ENV_FILE" ]; then
  : > "$ENV_FILE"
fi

CERT_BASE64=$(base64_no_wrap < "$CERT_PATH")

if [ "$SET_GH_SECRETS" -eq 1 ]; then
  upload_secret APPLE_CERT_BASE64 "$CERT_BASE64"
  upload_secret APPLE_CERT_PASSWORD "$CERT_PASSWORD"
  upload_secret APPLE_SIGNING_IDENTITY "$SIGNING_IDENTITY"

  if [ "$HAS_NOTARIZATION" -eq 1 ]; then
    upload_secret APPLE_ID "$APPLE_ID"
    upload_secret APPLE_APP_PASSWORD "$APP_PASSWORD"
    upload_secret APPLE_TEAM_ID "$TEAM_ID"
  fi

  echo "Uploaded signing secrets to GitHub."
  if [ "$HAS_NOTARIZATION" -eq 1 ]; then
    echo "Uploaded notarization secrets to GitHub."
  else
    echo "Skipped notarization secrets because they were not provided."
  fi
  exit 0
fi

write_output "APPLE_CERT_BASE64=$CERT_BASE64"
write_output "APPLE_CERT_PASSWORD=$CERT_PASSWORD"
write_output "APPLE_SIGNING_IDENTITY=$SIGNING_IDENTITY"

if [ "$HAS_NOTARIZATION" -eq 1 ]; then
  write_output "APPLE_ID=$APPLE_ID"
  write_output "APPLE_APP_PASSWORD=$APP_PASSWORD"
  write_output "APPLE_TEAM_ID=$TEAM_ID"
fi

if [ -n "$ENV_FILE" ]; then
  echo "Wrote secrets to $ENV_FILE"
fi