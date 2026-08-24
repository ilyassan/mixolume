# Contributing to MiXolume

Thanks for your interest in contributing! MiXolume is a small, cross-platform
(Windows/macOS/Linux) desktop audio mixer built with [Tauri v2](https://v2.tauri.app/)
(Rust backend) and React/TypeScript (frontend).

## Setting up a dev environment

You'll need:

- **Node.js** (v20 or later) and **npm** for the frontend.
- **Rust** (stable toolchain, via [rustup](https://rustup.rs/)) and **cargo** for the backend.
- The platform-specific **Tauri prerequisites** for your OS (system WebView,
  build tools, etc.). Follow the official Tauri prerequisites guide rather
  than relying on this document, since these requirements evolve with Tauri
  itself: https://v2.tauri.app/start/prerequisites/

Once those are installed:

```sh
npm install
```

## Running the app in development

```sh
npm run tauri dev
```

This starts the Vite dev server and launches the Tauri window with hot reload.

## Running tests

Frontend (TypeScript) tests, from the repo root:

```sh
npm run test
```

Backend (Rust) tests, from `src-tauri/`:

```sh
cd src-tauri
cargo test
```

Please also make sure the following pass before opening a PR, as they are
required in CI:

```sh
npx tsc --noEmit
npm run lint
cargo check   # from src-tauri/
```

## Branching model

- `main` is the trunk — create a feature branch off `main` (e.g.
  `feat/per-app-volume-linux`, `fix/tray-icon-flicker`) and open your pull
  request against `main`. All PRs require CI to pass and at least one review
  before merging.
- We use **squash merges** into `main` — a PR becomes a single commit on
  `main`, so keep your PR title/description clean, as it becomes the squash
  commit message.
- `beta` is a prerelease checkpoint, not a place PRs target directly. When
  `main` is in a state worth letting early testers try before it's a full
  stable release, fast-forward `beta` to that commit and push a
  `vX.Y.Z-beta.N` tag from it. A stable release is a `vX.Y.Z` tag pushed
  directly from `main`. Either tag triggers
  [`.github/workflows/release.yml`](.github/workflows/release.yml), which
  marks the GitHub Release as a prerelease automatically whenever the tag
  contains a `-`.

## Commit message convention

We use lightweight, conventional-ish prefixes on commit subjects:

- `feat:` — a new user-facing feature
- `fix:` — a bug fix
- `chore:` — maintenance, tooling, dependency bumps, etc. with no user-facing change
- `docs:` — documentation-only changes
- `refactor:` — internal code change with no behavior change
- `test:` — adding or fixing tests only

Example: `fix: correct per-app mute state on Windows sleep/resume`

## Code style

- TypeScript/React: keep changes type-safe (`tsc --noEmit` must pass) and lint-clean (`npm run lint`).
- Rust: run `cargo fmt` and keep `cargo check` clean before submitting.

## Questions

If anything here is unclear, feel free to open an issue to ask.
