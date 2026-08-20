# macOS audio strategy: Core Audio Process Tap API (no third-party driver)

This directory replaces the now-deleted `src-tauri/macos-driver/` (see git history for that
earlier design). It documents the macOS backend (`src-tauri/src/mixer/macos.rs`), which uses
Apple's public **Core Audio Process Tap API** directly. There is no HAL driver, no third-party
dependency to install, and no GPL exposure.

## Why the previous approach was replaced

The prior design talked to an independently-installed `kyleneideck/BackgroundMusic` (GPLv2) HAL
driver's `kAudioDeviceCustomPropertyAppVolumes` property. That was a real, working mechanism, but
it had costs a native API avoids entirely:

- **Licensing friction.** BackgroundMusic is GPLv2. The previous design took care to only *talk to*
  an independently-installed, unmodified copy (a "mere aggregation" argument, not linking), but
  that reasoning had to be documented and defended at all -- a native Apple API needs none of it.
- **Install friction.** The user had to separately download and install a third-party, admin-level
  audio driver bundle into `/Library/Audio/Plug-Ins/HAL/` and restart `coreaudiod` (or log out) to
  pick it up, before Mixolume could do anything at all.
- **The routing problem was never actually solved.** BackgroundMusic's driver only *exposes* a
  virtual device; something still has to set that device as the system default output and forward
  the mixed audio through to the real speakers (BGMApp's job in the original project). The old
  `macos.rs` explicitly left "does Mixolume also need the user to run BGMApp, or does it have to
  reimplement BGMApp's routing role itself" as an **unresolved, undocumented-as-solved** open
  question. In practice this meant the old backend could read/write a volume property that had no
  guaranteed effect on anything actually audible.

The Core Audio Process Tap API sidesteps all three: it's a public Apple framework API (no GPL
code anywhere near Mixolume), it needs a one-time TCC permission prompt instead of a driver
install, and the routing story is fully specified (see "Architecture" below) rather than an open
question.

## The API being used

- **`CATapDescription` / `AudioHardwareCreateProcessTap`** (AudioToolbox/CoreAudio, part of the
  system frameworks) -- ships starting in **macOS 14.2 Sonoma**. Treat **14.4+ as the practical
  documented minimum**: every real-world reference implementation found during this research
  session targets 14.2 as the `LSMinimumSystemVersion` floor, but 14.4 is where the API matured
  enough that it's the version people actually build and ship against.
- **`CATapDescription.muteBehavior = .mutedWhenTapped`** (`CATapMuteBehavior` case
  `MutedWhenTapped`) is a genuine, real mute -- it routes a tapped process's audio to the tap *and*
  silences that process's normal path to the speakers for the duration of the read. This is not a
  "copy while still playing" tap; apps that are tapped truly stop being audible through their
  normal path.
- **`AudioHardwareCreateAggregateDevice`** -- plain public Core Audio API (no custom driver, no
  admin install) for combining multiple taps (plus the real output device, for clocking) into one
  aggregate device.
- **No special Apple-granted entitlement is needed.** The only user-facing gate is a standard TCC
  "System Audio Capture" permission prompt, triggered automatically the first time
  `AudioHardwareCreateProcessTap` actually runs, driven by an `NSAudioCaptureUsageDescription`
  string in the app's `Info.plist` -- no microphone permission, and it works fully outside the Mac
  App Store / unsandboxed. (Confirmed via the reference apps below; see "TCC permission UX".)

### Architecture (confirmed, not theoretical)

This session cloned and read two MIT-licensed reference projects in full rather than guessing API
shapes from memory:

- **[`altuzar/sonicflow`](https://github.com/altuzar/sonicflow)** -- a working per-app macOS volume
  mixer built on exactly this API. Its `Sources/SonicFlow/Audio/` directory is the direct
  structural model for `src/mixer/macos.rs`. Confirms the real architecture is **two IOProcs
  bridged by a lock-free ring buffer**, not a single aggregate device that somehow drives the
  physical output directly (Core Audio has no API for one device's IOProc to drive a *different*
  device's hardware output):
  1. A **private capture aggregate device** whose sub-devices are the per-app taps plus the real
     default output device (included only as a shared clock source). Its IOProc reads each tap's
     buffer, multiplies by that app's gain, mixes into a scratch buffer, and writes the result into
     a ring buffer.
  2. A **second IOProc installed directly on the real default output device**. It reads from the
     ring buffer and **adds** (not replaces) the gained/mixed samples into the device's own output
     buffer -- which already contains whatever the system mixer wrote for every non-tapped app.
     Non-tapped apps are therefore untouched and keep playing through the normal system mixer.
- **[`insidegui/AudioCap`](https://github.com/insidegui/AudioCap)** -- capture-focused, but
  documents the raw process-tap mechanics well (Apple's own docs are thin). Independently confirms
  the same `CATapDescription`/`AudioHardwareCreateProcessTap`/aggregate-device shape, plus the
  exact `kAudioAggregateDeviceTapListKey`/`kAudioSubTapUIDKey`/`kAudioSubTapDriftCompensationKey`
  dictionary keys needed to wire a tap into an aggregate device.

Both repos were fetched and read in full (not summarized from search results) before any Rust code
was written. See `src/mixer/macos.rs`'s module doc comment for line-by-line citations of which
Swift file in each repo backs which piece of the Rust port, plus the exact `objc2-core-audio`
0.3.2 Rust function/struct signatures confirmed via `docs.rs` during the same research pass.

### TCC permission UX

Confirmed via sonicflow's `Resources/Info.plist` (`NSAudioCaptureUsageDescription` present,
`LSMinimumSystemVersion` = `14.2`) and `Services/PermissionsManager.swift` (no explicit TCC
preflight/request call for audio capture -- only used there for an unrelated Accessibility
permission): the audio-capture permission prompt fires automatically, driven purely by the
`NSAudioCaptureUsageDescription` Info.plist string, the first time `AudioHardwareCreateProcessTap`
actually executes. No dedicated preflight call is required for the default UX. (AudioCap's
`ProcessTap/AudioRecordingPermission.swift` shows an *optional* private `TCC.framework` SPI path
via `dlopen`/`dlsym`, gated behind a non-default `ENABLE_TCC_SPI` build flag, for apps that want to
preflight/request explicitly -- confirming that path is optional/unstable, not required.)

**Open TODO, not resolved in this session:** whether Tauri v2's `tauri.conf.json` bundle config
supports adding `NSAudioCaptureUsageDescription` to the generated macOS `.app`'s `Info.plist`
directly. A documentation fetch during this session indicated Tauri v2 exposes
`bundle.macOS.infoPlist` ("path to an Info.plist file to merge with the default Info.plist," with
`Info.plist`/`Info.macos.plist` also auto-detected if placed in the `src-tauri` config directory)
and a separate `bundle.macOS.entitlements` key for an entitlements file -- but this was not cross-
checked against a second source or an actual `tauri-cli` build in this session, so treat it as a
lead, not a confirmed fact. **Whoever wires up the real macOS bundle config needs to**: add
`NSAudioCaptureUsageDescription` (with a short, honest description of what the app does, e.g.
"Mixolume taps system audio output to apply per-app volume control. No audio is recorded,
transmitted, or stored." -- sonicflow's own wording, which is accurate for this project's design
too) via whichever of those mechanisms actually works, and confirm the prompt fires on a real
build before shipping.

## Version floor and what it means for users

`LSMinimumSystemVersion` should be set to (at least) **14.2**, matching sonicflow's own
`Info.plist`, but this project should document its actual supported floor as **14.4+**: every
reference implementation found targets 14.2 as a hard minimum while clearly treating 14.4 as the
version the API is actually solid on in practice. Concretely: **users on macOS 14.0/14.1, and
anything older than 14.2, cannot use per-app volume control on Mixolume at all** -- there is no
fallback to the old driver-based approach (deliberately not kept; see "Why the previous approach
was replaced" above). Mixolume's macOS backend should detect this at startup and surface a clear,
specific "requires macOS 14.4 or later" message rather than a generic Core Audio error, though that
UX work is not part of this backend rewrite and is a separate follow-up.

## What has NOT been verified

**None of `src/mixer/macos.rs` has been compiled or run.** This entire rewrite was researched and
written on a Windows machine with no access to Xcode, a Mac, or the Core Audio frameworks. The two
reference repos were cloned and read in full (real, verifiable source, not paraphrased from
memory), and the `objc2-core-audio`/`objc2-core-audio-types`/`objc2-foundation`/`block2` Rust crate
surface was checked against `docs.rs` where possible -- but a `docs.rs` page fetched through a
summarizing tool is not the same confidence level as a real `cargo build`. See
`src/mixer/macos.rs`'s module doc comment for the full, specific list of "highest-risk spots a
Mac-equipped contributor should check first" -- in short: (1) the exact `block2::RcBlock`
construction/coercion into Core Audio's raw `AudioDeviceIOBlock` callback type, (2)
`CATapMuteBehavior`'s exact Rust-side associated-constant spelling, and (3) the
`objc2-core-foundation` `CFDictionary`/`CFArray` construction calls used to build the aggregate
device's description dictionary.

### What a Mac-equipped contributor needs to do

1. `cargo build` inside `src-tauri/` on macOS 14.4+ with Xcode installed, and work through whatever
   doesn't compile in `mixer/macos.rs` first -- start with the three risk items above.
2. Wire up `NSAudioCaptureUsageDescription` in the actual `.app` bundle's `Info.plist` (see "TCC
   permission UX" above) and confirm the permission prompt fires on first run.
3. With some app (a browser tab playing audio, `afplay`, etc.) producing sound, call
   `MacosMixerBackend::list_sessions()` and confirm it returns that process, confirm `set_volume`/
   `set_muted` audibly change its volume, and confirm apps that are *not* tapped are completely
   unaffected (still playing at normal volume through the system mixer).
4. Confirm the "rebuild the whole tap engine when the active-process set changes" behavior
   (documented in `mixer/macos.rs`'s module doc comment as a deliberate, confirmed-in-the-reference-
   architecture tradeoff) doesn't produce an objectionable audio glitch in practice -- if it does,
   that's a real product question for the maintainer, not a bug in this port specifically.
5. Please update this file with what you actually observe -- it will likely disagree with something
   above in the details, same as every "unverified, written on a machine without the target OS"
   scaffold in this repo before it.
