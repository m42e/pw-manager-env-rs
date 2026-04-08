# Resolution model

pw-env classifies every `.env` entry before it decides whether the value should be exported as-is, resolved from a
backend, or ignored for migration.

## Entry types

| Entry form | Classification | Result |
| --- | --- | --- |
| `KEY=` | Empty | Resolve from the configured default backend |
| `KEY=op://vault/item/field` | 1Password reference | Always resolve through 1Password |
| `KEY=bw://folder/item/field` | Bitwarden reference | Always resolve through Bitwarden |
| `KEY=plaintext` | Plaintext | Leave as-is until migrated |

Quoted values are unwrapped for classification, but pw-env preserves the raw line when it rewrites `.env` during
migration.

## Resolution flow

1. Parse `.env` and classify each entry.
2. If at least one entry needs backend resolution, confirm that secret fetching is approved for the project.
3. Detect the project name from the nearest Git repository root. If no Git root is found, use the current directory
   name.
4. For each resolvable entry, check the resolved-secret cache if caching is enabled.
5. Send cache misses for `op://...` references to 1Password.
6. Send cache misses for `bw://...` references to Bitwarden.
7. Send cache misses for empty values to the configured default backend.
8. Write successful backend results back to the cache.
9. Export only the keys that resolved successfully.

## Resolved-secret cache

When `[defaults.cache]` or a project-level `[cache]` block enables caching, pw-env stores resolved secret values in the
OS keyring when that secure store is available. The default cache lifetime is 4 hours.

Cache lookups are scoped to the `.env` entry and the effective lookup context, including the selected backend and the
backend settings that influence how the secret is resolved. Changing the entry or its effective backend settings causes
pw-env to miss the old cache entry and fetch a fresh value.

If the OS keyring is unavailable, pw-env logs that caching is disabled for the current run and continues resolving
secrets directly from the backend.

## Backend-specific behavior

### 1Password and Bitwarden

These backends resolve entries one key at a time. Explicit references bypass the default backend selection.

### GPG

The GPG backend decrypts the configured encrypted env file once, then pulls the requested empty keys out of the
decrypted content. With caching enabled, pw-env skips that decrypt when every requested GPG-backed key is already in
the resolved-secret cache.

## Partial failures are nonfatal

If one key fails to resolve, pw-env warns and keeps going. Successfully resolved keys are still exported.

That makes `pw-env load .` the best inspection command when a project is only partially configured.

## Audit logging

When log file output is configured, successful credential fetches are written as `AUDIT credential_fetch ...` lines. The
log includes the project, working directory, `.env` path, backend, and key name, but never the secret value itself.
