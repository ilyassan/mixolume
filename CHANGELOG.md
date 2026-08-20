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
- macOS backend: scaffolded against an independently-installed
  BackgroundMusic `BGMDriver`'s `kAudioDeviceCustomPropertyAppVolumes` HAL
  property; unverified pending access to real Mac hardware, and gated on an
  open licensing/product decision (see `src-tauri/macos-driver/README.md`).
- Mixer UI: per-session icon, name, volume slider, mute toggle; inactive
  sessions de-emphasized; removed sessions fade out instead of disappearing
  instantly.
- CI (paths-filtered frontend/backend jobs across Windows/macOS/Linux),
  Dependabot, issue/PR templates, contributing guide, security policy, code
  of conduct, and license.
