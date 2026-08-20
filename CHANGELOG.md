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
  bridged via a lock-free ring buffer to the real output device. Unverified
  pending access to real Mac hardware (see `src-tauri/macos-audio/README.md`).
  Replaces an earlier BackgroundMusic (GPLv2)-dependent design.
- Mixer UI: per-session icon, name, volume slider, mute toggle; inactive
  sessions de-emphasized; removed sessions fade out instead of disappearing
  instantly.
- CI (paths-filtered frontend/backend jobs across Windows/macOS/Linux),
  Dependabot, issue/PR templates, contributing guide, security policy, code
  of conduct, and license.
