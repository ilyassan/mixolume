# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Tauri v2 + React/TypeScript/Tailwind v4 app shell with a hidden,
  tray-toggled popover window.
- Shared `AudioMixerBackend` trait and `AppSession` model used by all three
  platform backends and the frontend.
- Windows backend: real WASAPI session enumeration, per-app volume/mute via
  `ISimpleAudioVolume`, process name + icon resolution.
- Linux backend: `pactl`-based sink-input volume/mute (MVP; libpulse FFI is a
  planned follow-up).
- macOS backend: built on Apple's Core Audio Process Tap API
  (`CATapDescription`/`AudioHardwareCreateProcessTap`, macOS 14.2+/14.4+
  practical minimum) — no third-party driver, no GPL dependency, no admin
  install. Per-app taps with a real mute of the normal output path, mixed and
  bridged via a lock-free ring buffer to the real output device. Verified
  live against real audio on real Mac hardware (see
  `src-tauri/macos-audio/README.md`). Replaces an earlier BackgroundMusic
  (GPLv2)-dependent design.
- Per-app independent left/right stereo balance control, on top of the
  existing per-app volume — verified live on macOS; implemented for
  Windows (`IChannelAudioVolume`) and Linux (`pactl`'s per-channel volume)
  but not yet run against real hardware on those platforms.
- Mixer UI: per-session icon, name, volume slider, mute toggle; inactive
  sessions de-emphasized; removed sessions fade out instead of disappearing
  instantly.
- Settings view: launch-at-login toggle, app version and branding footer.
- A dedicated in-app view explaining the macOS Screen & System Audio
  Recording permission when it hasn't been granted yet, instead of silently
  showing no sessions.
- New app icon: a single audio-waveform stroke traced as the letter M
  (`app-icon.svg` is the master source; `src-tauri/icons/` is regenerated
  from it via `npx tauri icon app-icon.svg`).
- CI (paths-filtered frontend/backend jobs across Windows/macOS/Linux) now
  also runs on the `beta` branch, plus Dependabot, issue/PR templates,
  contributing guide, security policy, code of conduct, and license.
- Tag-triggered cross-platform release workflows
  (`.github/workflows/release-{macos,windows,linux}.yml`, one file per
  platform) building signed-but-not-yet-notarized macOS, unsigned Windows,
  and unsigned Linux installers in parallel and attaching them all to the
  same draft GitHub Release.
- `main`/`beta` branching model: `main` is the trunk; `beta` is a
  fast-forwarded prerelease checkpoint for `vX.Y.Z-beta.N` tags.
- **Auto-duck** (macOS, Windows): pick which apps should automatically lower
  every other app's volume while they're actually talking, driven by real
  speech detection (WebRTC's VAD) rather than just "is this app open."
  Opt-in per app via the Settings ducking list, which persists each app's
  icon so it keeps showing correctly even after the app quits.
- **Volume boost** past 100% on macOS, for a session that's still too quiet
  at its normal ceiling.
- **Per-app output device routing** (Windows, then macOS): send one app's
  audio to a specific output device (e.g. headphones) while everything else
  follows the system default.
- **Auto-update**: checks for a new version at launch and via a "Check for
  Updates" button in Settings, downloads and verifies it against a signed
  manifest, and installs on the next relaunch. Beta and stable installs
  update independently on their own channels -- a beta never jumps to
  another beta's channel mixing with stable, and vice versa (see
  `CONTRIBUTING.md`).
- A single-instance guard: launching MiXolume again while it's already
  running re-focuses the existing window instead of starting a second
  process (which would otherwise fight over the same audio sessions).
- A dedicated monochrome tray icon for macOS's menu bar, matching the
  platform's own template-image convention for menu bar extras, instead of
  reusing the full-color app icon.

### Changed

- Brand name unified to **MiXolume** everywhere the OS or app surfaces it —
  window title, dock/tray tooltip, tray menu, macOS permission prompt text,
  bundle product name.
- Session rows redesigned: mute and expand controls moved into the title row
  so the volume slider spans the full row width; per-app balance now exposed
  as two independent left/right sliders (in a collapsible "advanced" panel)
  instead of a single balance slider.
- Multiple audio sessions from the same app (e.g. several Zoom sessions)
  merge into a single controllable row on Windows instead of listing each
  one separately.
- Full app metadata (publisher, copyright, category, description) now set
  in the bundle config, instead of the unedited Tauri project scaffold
  defaults — affects what Windows shows in Add/Remove Programs and what
  macOS shows in Spotlight/Launch Services.
- Officially published platforms are macOS and Windows; Linux remains
  fully implemented and unit-tested but is built from source only for now
  (see `CONTRIBUTING.md`).

### Fixed

- A rendering bug where opening devtools broke the WKWebView on macOS.
- The app terminating itself automatically instead of staying resident in
  the tray.
- Window transparency not actually applying on macOS (needs
  `macOSPrivateApi: true` alongside `transparent: true`).
- Tray-anchored window position drifting after the tray icon moved.
- macOS code-signing instability: ad-hoc signing gave every local rebuild a
  new TCC identity, causing repeated permission prompts and unreliable
  behavior that looked like random breakage. Fixed locally with a stable
  self-signed development certificate, and carried the same fix into CI
  releases with a separate release-only certificate.
- A rebuild loop on macOS where the app's own audio tap was capturing its
  own output, starving the real-time playback IOProc.
- Volume/balance slider drag flicker, and a delay before auto-duck reacted
  to its first trigger.
- The Settings ducking panel showing its list instantly/glitching instead
  of animating in smoothly; ducking settings now prefetch at app launch
  instead of on-demand when Settings opens.
- An app icon with no representation near the intended small size (only,
  say, 16px and a 1024px master) encoding a full-resolution PNG well over a
  megabyte, which — persisted as JSON — could bloat the settings file past
  10MB and make the app appear stuck on launch while parsing it. Icons are
  now resized down before encoding, and both the per-icon and total
  settings-file size are capped as a second line of defense.
- The macOS and Windows release notes drifting out of sync with each
  other (and with reality) since each platform's release workflow wrote
  its own copy of the shared GitHub Release's body.
