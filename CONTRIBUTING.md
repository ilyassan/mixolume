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
- There's no separate release branch — both a prerelease and a stable
  release are tags pushed directly from whatever commit on `main` is ready
  to ship. Push a `vX.Y.Z-beta.N` tag for an early-tester build, or a plain
  `vX.Y.Z` tag for a stable release. Either one triggers
  `.github/workflows/release-macos.yml` and `release-windows.yml` (one file
  per platform so each one's build/re-run status is independent), which
  build in parallel and attach their installers to the same GitHub Release
  — marked as a prerelease automatically whenever the tag contains a `-`.
  `release-linux.yml` is manual-only (`workflow_dispatch`) for now: Linux
  isn't an officially published platform yet, see the README.
- Releases are created as **drafts** — check the assets and release notes
  on GitHub once both platform workflows finish, then publish it by hand.
  This is a deliberate manual gate, not a bug: nothing is visible to users
  (and `tauri-plugin-updater`'s "latest release" endpoint can't resolve to
  it) until it's published.
- Auto-update has two independent channels, each pointed at its own build.
  A stable build's updater endpoint (baked in at build time from
  `tauri.conf.json`) is GitHub's `releases/latest/download/latest.json` --
  which only ever resolves to a published, non-prerelease release, so a
  stable install can never silently jump onto a beta. A beta build gets a
  different endpoint patched in during CI (see `release-macos.yml`'s/
  `release-windows.yml`'s "Point the updater at the beta manifest" step),
  pointed at a fixed, permanently-published `latest-beta` release that
  `generate-update-manifest` overwrites on every new beta tag -- since
  every beta tag is its own separate release, unlike stable's `latest`
  shortcut, that fixed release is the only way a beta install has a
  constant URL to check.

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
