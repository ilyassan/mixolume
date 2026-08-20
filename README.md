# Mixolume

A small cross-platform desktop utility that lists every application currently producing sound
and gives you an independent volume slider for each one — instead of one system-wide volume
control.

Windows has this natively (and the excellent third-party [EarTrumpet](https://eartrumpet.app/)).
macOS has [SoundSource](https://rogueamoeba.com/soundsource/) and a few free alternatives. Linux
has `pavucontrol`. Nothing ships one polished app that does this on **all three** from a single
codebase — that's the gap Mixolume fills.

> **Status:** early, actively-developed. The Windows backend is real and functional (WASAPI
> session enumeration + per-app volume/mute). The Linux backend (PulseAudio via `pactl`) is
> implemented but has only been syntax/unit-test verified on this machine, not run against a
> real PulseAudio install yet. The macOS backend is a documented, unverified scaffold — see
> [`src-tauri/macos-driver/README.md`](src-tauri/macos-driver/README.md) for why, and for a real
> open licensing/product question that needs a decision before macOS support is finished.

## How it works

- **Windows:** talks directly to WASAPI's per-application audio sessions
  (`IAudioSessionManager2` → `ISimpleAudioVolume`). No driver, no elevated privileges.
- **Linux:** shells out to `pactl` to read/set PulseAudio (or PipeWire's `pipewire-pulse`
  compat shim) sink-input volumes. No elevated privileges.
- **macOS:** has no public per-app volume API at all. Mixolume talks to an independently
  installed [BackgroundMusic](https://github.com/kyleneideck/BackgroundMusic) `BGMDriver`
  virtual audio device via the public Core Audio HAL property `kAudioDeviceCustomPropertyAppVolumes`
  — Mixolume does not vendor or fork BackgroundMusic's (GPLv2) source. Full rationale in
  [`src-tauri/macos-driver/README.md`](src-tauri/macos-driver/README.md).

One Tauri app, one repo, one React UI shared across all three platforms — see
[`PLAN.md`](PLAN.md) for the full architecture writeup.

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

Mixolume does not collect any analytics in its current state. If that changes, this section
(and an in-app disclosure) will say exactly what is and isn't collected before it ships — see
`PLAN.md` section 8 for the stance that will govern any future addition.

## License

[MIT](LICENSE) for Mixolume's own code. macOS support depends on a separately-installed,
unmodified GPLv2 component (BackgroundMusic) that Mixolume does not redistribute — see
[`src-tauri/macos-driver/README.md`](src-tauri/macos-driver/README.md).
