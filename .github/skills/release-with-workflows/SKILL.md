---
name: release-with-workflows
description: 'Create a new pw-env release using this repository''s PR-driven GitHub workflows. Use when preparing a release PR, reviewing release notes, merging a release PR, confirming automatic tagging, checking release artifacts, or setting up macOS signing and notarization secrets.'
argument-hint: 'Provide the target version for the release PR workflow, for example 0.2.2.'
user-invocable: true
---

# Release With Workflows

Use this skill to prepare and publish a new `pw-env` release from this repository.

The repository publishes releases through a PR-driven workflow:

1. Dispatch `prepare-release-pr.yml` with a semver version like `0.2.2`.
2. Review and merge the generated `release/v<version>` PR.
3. `tag-release-pr.yml` creates tag `v<version>` and dispatches `release.yml`.

## When to Use

- Prepare a new release version.
- Follow the repository's preferred release path.
- Verify release notes, version bumps, tags, and published artifacts.
- Configure optional macOS signing and notarization for release artifacts.

## Inputs To Confirm

- Target version in semver format, without a leading `v` when preparing the release.
- Whether macOS signing or notarization must be active for this release.

## Decision Points

### Use the PR-driven workflow by default

- The repository's preferred release path is `prepare-release-pr.yml` followed by `tag-release-pr.yml` and `release.yml`.
- Only switch to a manual tag-driven release if the user explicitly requests it.

### Choose signing and notarization behavior

- If `APPLE_CERT_BASE64`, `APPLE_CERT_PASSWORD`, and `APPLE_SIGNING_IDENTITY` are set as GitHub secrets, macOS binaries are signed.
- If those signing secrets are missing, macOS binaries are still built and published, but unsigned.
- If `APPLE_ID`, `APPLE_APP_PASSWORD`, and `APPLE_TEAM_ID` are also set, signed macOS binaries are notarized before packaging.

## Procedure

1. Verify release preconditions.
   - Confirm the version is valid semver, for example `0.2.2`.
   - Confirm tag `v<version>` does not already exist.

2. Prepare the release PR.
   - Run `gh workflow run prepare-release-pr.yml --field version=<version>`.
   - Wait for the workflow to open PR `release/v<version>`.
   - Review `Cargo.toml`, `Cargo.lock`, and `release-notes/v<version>.md` in the PR.
   - Confirm the PR title is `Release v<version>` and it still carries the `release` label.

3. Complete the trigger that starts publishing.
   - Merge the release PR into `main` after review and CI pass.
   - Confirm `tag-release-pr.yml` runs on the merged PR.
   - Confirm it creates tag `v<version>` and dispatches `release.yml`.

4. Monitor the publish workflow.
   - Confirm `release.yml` runs for tag `v<version>`.
   - Confirm builds complete for:
     - `x86_64-unknown-linux-gnu`
     - `aarch64-unknown-linux-gnu`
     - `x86_64-pc-windows-msvc`
     - `x86_64-apple-darwin`
     - `aarch64-apple-darwin`
   - Confirm the workflow creates the GitHub release and uploads all built artifacts.

5. Verify release correctness.
   - Confirm the crate version in `Cargo.toml` matches tag version without the leading `v`.
   - Confirm release notes are present and match the release content.
   - Confirm the GitHub release title is `v<version>`.
   - Confirm artifacts exist for every target.
   - If signing was required, confirm macOS signing steps executed.
   - If notarization was required, confirm the notarization step executed successfully.

## macOS Secrets Setup

Use `./scripts/setup-apple-signing-secrets.sh` to prepare or upload GitHub secrets.

- Signing only:
  - Provide `--cert`, `--cert-password`, and `--identity`.
- Signing plus notarization:
  - Also provide `--apple-id`, `--app-password`, and `--team-id`.
- To upload directly to GitHub, add `--set-gh-secrets` and optionally `--repo owner/repo`.

Example:

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

## Completion Checks

- Release version bump is present and correct.
- Tag `v<version>` exists exactly once and points to the intended release commit.
- `release.yml` completed successfully.
- GitHub release exists for `v<version>` with generated notes and uploaded artifacts.
- macOS signing and notarization behavior matches the configured secrets.

## Repository Facts Used By This Skill

- Default branch is `main`.
- Automated release preparation uses `.github/workflows/prepare-release-pr.yml`.
- Tagging after merge uses `.github/workflows/tag-release-pr.yml`.
- Publishing uses `.github/workflows/release.yml`.
