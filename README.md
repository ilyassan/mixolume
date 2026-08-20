# Mixolume

A small cross-platform desktop utility that lists every application currently producing sound
and gives you an independent volume slider for each one — instead of one system-wide volume
control.

Windows has this natively (and the excellent third-party [EarTrumpet](https://eartrumpet.app/)).
macOS has [SoundSource](https://rogueamoeba.com/soundsource/) and a few free alternatives. Linux
has `pavucontrol`. Nothing ships one polished app that does this on **all three** from a single
codebase — that's the gap Mixolume fills.

> **Status:** early, actively-developed. The Windows backend is real and functional (WASAPI
> session enumeration + per-app volume/mute), verified live against real audio on a dev machine.
> The Linux backend (PulseAudio via `pactl`) is implemented but has only been syntax/unit-test
> verified, not run against a real PulseAudio install yet. The macOS backend is written against
> Apple's Core Audio Process Tap API but entirely unverified — no Mac was available during
> development. See [`src-tauri/macos-audio/README.md`](src-tauri/macos-audio/README.md) for the
> full architecture and what a Mac-equipped contributor needs to check first.

## How it works

- **Windows:** talks directly to WASAPI's per-application audio sessions
  (`IAudioSessionManager2` → `ISimpleAudioVolume`). No driver, no elevated privileges.
- **Linux:** shells out to `pactl` to read/set PulseAudio (or PipeWire's `pipewire-pulse`
  compat shim) sink-input volumes. No elevated privileges.
- **macOS:** has no public per-app volume API at all, so Mixolume uses Apple's Core Audio
  **Process Tap** API (`CATapDescription` / `AudioHardwareCreateProcessTap`, macOS 14.2+) to tap
  each app's audio with a real mute of its normal output path, mix in a per-app gain, and feed the
  result back to the real output device via a private aggregate device + lock-free ring buffer.
  No third-party driver, no admin install, no GPL dependency — just a one-time system permission
  prompt. Full rationale, citations, and version-floor trade-offs in
  [`src-tauri/macos-audio/README.md`](src-tauri/macos-audio/README.md).

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

[MIT](LICENSE) for Mixolume's own code. The macOS backend uses only public Apple frameworks —
no third-party or GPL-licensed component involved.
