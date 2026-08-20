## What changed

<!-- Summarize the change and why it's needed. Link related issues (e.g. "Closes #123"). -->

## Why

<!-- The motivation / context behind this change. -->

## How to test

<!-- Steps a reviewer can follow to verify the change locally. -->

## Checklist

- [ ] `npx tsc --noEmit` passes
- [ ] `npx vitest run` (frontend tests) passes, if frontend code changed
- [ ] `cargo test` (run from `src-tauri/`) passes, if backend code changed
- [ ] `cargo check` (run from `src-tauri/`) passes, if backend code changed
- [ ] Documentation updated, if relevant (`README.md`, `CONTRIBUTING.md`, etc.)
- [ ] `CHANGELOG.md` updated under `[Unreleased]`, if user-facing
