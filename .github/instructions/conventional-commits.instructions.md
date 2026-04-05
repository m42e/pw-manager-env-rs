---
description: "Use when creating commits, suggesting commit messages, preparing pull requests, or describing git history. Enforces Conventional Commits 1.0.0 commit summaries."
name: "Conventional Commits"
---
# Conventional Commits

- When creating a commit or suggesting a commit message, use the Conventional Commits 1.0.0 summary format: `<type>[optional scope]: <description>`.
- Keep the summary in the imperative mood and describe the change, not the intent to change it.
- Use a lowercase type from the Conventional Commits spec such as `feat`, `fix`, `docs`, `refactor`, `test`, `build`, `ci`, `chore`, `perf`, or `revert`.
- Add a scope when it improves clarity, for example `fix(cli): handle empty history output`.
- Mark breaking changes with `!` in the summary when appropriate, for example `feat(api)!: remove legacy upload endpoint`.
- Keep the summary concise and avoid trailing punctuation.
- If a fuller explanation is useful, place it in the commit body instead of overloading the summary.
- If the requested commit message does not fit the spec cleanly, ask for the missing context instead of inventing a misleading type or scope.