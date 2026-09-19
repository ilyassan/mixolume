# MiXolume

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/ilyassan/mixolume)](https://github.com/ilyassan/mixolume/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/ilyassan/mixolume/total.svg)](https://github.com/ilyassan/mixolume/releases)

<div align="center">
  <img src="app-icon.svg" alt="MiXolume logo" width="128" height="128">
</div>

A menu-bar utility that lists every application currently producing sound and gives you an
independent volume slider for each one, instead of one system-wide volume control.

Windows has this natively (and the excellent third-party [EarTrumpet](https://eartrumpet.app/)).
macOS has [SoundSource](https://rogueamoeba.com/soundsource/) and a few free alternatives. Nothing
ships one polished, free app that does this across platforms from a single codebase — that's the
gap MiXolume fills.

## Download

Grab the latest build from the [Releases page](https://github.com/ilyassan/mixolume/releases/latest):

- **macOS** — `MiXolume_universal.dmg` (Apple Silicon + Intel, signed and notarized, macOS 14.4+)
- **Windows** — `MiXolume_x64-setup.exe` (unsigned — Windows SmartScreen may warn on first run;
  click **More info → Run anyway**)

MiXolume checks for updates automatically and installs them in the background — no need to
re-download manually after the first install.

## Features

- **Independent per-app volume**, with a slider that goes past 100% on macOS for a quiet app.
- **Per-app left/right balance**, on top of volume.
- **Per-app output device routing** (macOS, Windows) — send one app's audio to your headphones
  while everything else stays on your speakers.
- **Auto-duck**: pick which apps (Zoom, Discord, FaceTime, etc.) should automatically lower every
  other app's volume while they're actually talking — driven by real speech detection, not just
  "is this app open."
- **Launch at login**, lives in the tray/menu bar, and stays out of your way otherwise.

## How it works

- **Windows:** talks directly to WASAPI's per-application audio sessions
  (`IAudioSessionManager2` → `ISimpleAudioVolume`). No driver, no elevated privileges.
- **macOS:** has no public per-app volume API at all, so MiXolume uses Apple's Core Audio
  **Process Tap** API (`CATapDescription` / `AudioHardwareCreateProcessTap`, macOS 14.2+) to tap
  each app's audio with a real mute of its normal output path, mix in a per-app gain, and feed the
  result back to the real output device via a private aggregate device + lock-free ring buffer.
  No third-party driver, no admin install, no GPL dependency — just a one-time system permission
  prompt. Full rationale, citations, and version-floor trade-offs in
  [`src-tauri/macos-audio/README.md`](src-tauri/macos-audio/README.md).
- **Linux:** implemented (shells out to `pactl`/PipeWire's compat shim) and unit-tested, but not
  yet an officially published/supported platform — build it yourself from source if you'd like to
  try it (see below).

One Tauri app, one repo, one React UI shared across platforms — see [`PLAN.md`](PLAN.md) for the
full architecture writeup.

## Development

Prerequisites: Node.js 20+, Rust (stable), and Tauri's per-OS
[system prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
npm install
npm run tauri dev
```

Tests:

```bash
npm run test              # frontend: vitest
cd src-tauri && cargo test # backend: platform-specific modules only compile on their own OS
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the branching model and commit conventions.

## Privacy

MiXolume does not collect any analytics. If that changes, this section (and an in-app disclosure)
will say exactly what is and isn't collected before it ships — see `PLAN.md` section 8 for the
stance that will govern any future addition.

## License

[MIT](LICENSE) for MiXolume's own code. The macOS backend uses only public Apple frameworks —
no third-party or GPL-licensed component involved.
