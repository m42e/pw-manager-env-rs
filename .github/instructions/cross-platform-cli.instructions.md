---
name: "Cross-Platform CLI Changes"
description: "Use when adding or changing CLI features, shell integration, install scripts, tests, or user-facing docs in this multi-OS project. Covers Windows, macOS, and Linux compatibility checks."
applyTo: ["src/**", "tests/**", "examples/**", "scripts/**", "docs/**", "public/**", "site/**"]
---

# Cross-Platform CLI Changes

- Treat every feature and test change as a Windows, macOS, and Linux change unless the user explicitly limits scope to one platform.
- Check shell-specific behavior any time command output, hooks, quoting, or environment activation changes. This project supports Unix shells and PowerShell, so syntax and behavior often differ.
- Prefer platform-neutral path and process handling. Avoid assumptions about separators, executable lookup, file permissions, or shell availability.
- When behavior is intentionally platform-specific, isolate it clearly with the appropriate Rust cfg gates and preserve the non-target platforms' behavior.
- Add or update tests to cover platform differences when feasible. If a test cannot run on one platform, gate or ignore it explicitly and state why in the test.
- Keep assertions resilient across platforms. Do not depend on Unix-only permissions, shell syntax, or path formatting unless the test is explicitly scoped to that platform.
- When changing install, init, export, or shell-integration flows, verify the user experience for Windows, macOS, and Linux separately instead of assuming one platform's behavior generalizes.
- When updating docs for setup or CLI usage, include platform-specific guidance when commands, shells, or install steps differ.
