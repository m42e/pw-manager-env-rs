# Release Workflow

This repository publishes releases from tags that match `v*` through the GitHub Actions workflow in `.github/workflows/release.yml`.

## Release Steps

1. Prepare the release version locally.

```bash
./scripts/release.sh 0.1.1
```

2. Review the generated commit and release notes.

3. Push `main` and the annotated tag.

```bash
git push origin main v0.1.1
```

4. GitHub Actions builds the release artifacts for Linux, Windows, and macOS.

5. The workflow creates the GitHub release and uploads the build artifacts.

## macOS Signing and Notarization

The macOS release artifacts can be signed and notarized directly in GitHub Actions.

The workflow behaves like this:

- If the signing secrets are missing, macOS artifacts are built and published unsigned.
- If the signing secrets are present, the `pw-env` binary is signed before packaging.
- If the notarization secrets are also present, the signed binary is submitted to Apple notarization without waiting for Apple to finish processing.
- The release is published immediately after submission, and `.github/workflows/finalize-notarization.yml` polls Apple hourly.
- Pending notarization submissions are stored as GitHub Actions artifacts instead of being embedded in the GitHub release notes.
- Once Apple accepts the submission, the polling workflow staples the macOS binaries and replaces the existing macOS release assets in place.

### Required GitHub Secrets

Required for signing:

- `APPLE_CERT_BASE64`: Base64-encoded `.p12` export of your Developer ID Application certificate.
- `APPLE_CERT_PASSWORD`: Password used when exporting the `.p12` file.
- `APPLE_SIGNING_IDENTITY`: Full signing identity string used by `codesign`.

Optional, only for notarization:

- `APPLE_ID`: Apple ID email address used for notarization.
- `APPLE_APP_PASSWORD`: App-specific password created for that Apple ID.
- `APPLE_TEAM_ID`: Apple Developer Team ID.

## How To Obtain The Apple Secrets

### 1. Create or download a Developer ID Application certificate

You need an active Apple Developer membership with access to Developer ID certificates.

1. Open the Apple Developer Certificates portal.
2. Create a `Developer ID Application` certificate if you do not already have one.
3. Download the certificate and install it into Keychain Access on your Mac.

### 2. Export the certificate as `.p12`

1. Open Keychain Access.
2. Find the installed `Developer ID Application` certificate.
3. Export it as a `.p12` file.
4. Choose a strong export password. That password becomes `APPLE_CERT_PASSWORD`.

### 3. Find the signing identity string

Run this on the Mac where the certificate is installed:

```bash
security find-identity -v -p codesigning
```

Use the full `Developer ID Application: ...` value as `APPLE_SIGNING_IDENTITY`.

### 4. Find your Apple Team ID

Open the Apple Developer membership page and copy the Team ID. This becomes `APPLE_TEAM_ID`.

### 5. Create an app-specific password for notarization

If you want notarization, sign in to the Apple ID account page and create an app-specific password.

That password becomes `APPLE_APP_PASSWORD`, and the Apple ID email becomes `APPLE_ID`.

## Helper Script

Use the helper script to prepare the secrets or upload them directly with GitHub CLI:

```bash
./scripts/setup-apple-signing-secrets.sh \
  --cert ~/Certificates/developer-id-application.p12 \
  --cert-password 'export-password' \
  --identity 'Developer ID Application: Example Corp (TEAMID1234)' \
  --apple-id 'developer@example.com' \
  --app-password 'abcd-efgh-ijkl-mnop' \
  --team-id 'TEAMID1234' \
  --repo m42e/pw-manager-env-rs \
  --set-gh-secrets
```

If you only want signing and not notarization yet, omit `--apple-id`, `--app-password`, and `--team-id`.

To inspect the generated values without uploading them:

```bash
./scripts/setup-apple-signing-secrets.sh \
  --cert ~/Certificates/developer-id-application.p12 \
  --cert-password 'export-password' \
  --identity 'Developer ID Application: Example Corp (TEAMID1234)'
```

To write the generated values to a local env file instead of stdout:

```bash
./scripts/setup-apple-signing-secrets.sh \
  --cert ~/Certificates/developer-id-application.p12 \
  --cert-password 'export-password' \
  --identity 'Developer ID Application: Example Corp (TEAMID1234)' \
  --env-file .apple-signing-secrets.env
```

## Notes

- The release workflow signs the CLI binary itself, not an `.app` bundle.
- The script does not generate Apple credentials for you; it only packages and uploads values you already obtained from Apple.
- Treat the `.p12` export, export password, and app-specific password as sensitive credentials.