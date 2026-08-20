# macOS driver strategy: BackgroundMusic as an external dependency

This directory intentionally contains no vendored driver source. It exists to document the
legal and architectural reasoning behind Mixolume's macOS backend
(`src-tauri/src/mixer/macos.rs`), which talks to a virtual audio device published by
[`kyleneideck/BackgroundMusic`](https://github.com/kyleneideck/BackgroundMusic) ("BGMDriver")
that the **user installs separately**, not something this repo builds, forks, or ships.

## Why macOS needs a third-party driver at all

Unlike Windows (per-session `ISimpleAudioVolume` via WASAPI) and Linux (per-sink-input volume via
PulseAudio/PipeWire), **macOS has no public API for per-application output volume control.**
Core Audio's HAL only exposes volume at the level of physical/virtual *devices*, not individual
client processes. The only way to get real per-app volume on macOS today is to interpose a
virtual audio device that every app's audio gets routed through, and adjust each app's stream
inside that device's real-time IO cycle. Writing such a device (a Core Audio HAL plugin, i.e. a
kernel-adjacent driver-like component with strict real-time constraints) from scratch is a large,
high-risk undertaking. BackgroundMusic already does this well and is the de facto standard
solution the macOS indie-dev community relies on for this exact problem.

## The GPLv2 constraint

BackgroundMusic is licensed under the **GNU General Public License v2.0**. Its license (GPLv2
§2(b)) states:

> "You must cause any work that you distribute or publish, that in whole or in part contains or
> is derived from the Program or any part thereof, to be licensed as a whole at no charge to all
> third parties under the terms of this License."

If Mixolume vendored, forked, statically linked, or otherwise incorporated BGMDriver's (or
BGMApp's) source into this repository or into Mixolume's compiled binary, the resulting work
would be "derived from the Program" under §2(b), and **the whole of Mixolume would have to be
distributed under GPLv2** — source included, at no charge. That is not a constraint the
maintainer has agreed to for this project.

### The decided approach: mere aggregation, not a derivative work

GPLv2 §2's final paragraph carves out an explicit exception:

> "In addition, mere aggregation of another work not based on the Program with the Program (or
> with a work based on the Program) on a volume of a storage or distribution medium does not
> bring the other work under the scope of this License."

Mixolume's approach, **already decided and not up for re-litigation in this file**: treat an
independently-installed, unmodified BGMDriver the same way an application might treat a
system-installed `ffmpeg` binary it shells out to, rather than a library it statically links --
as a separate program the user installs on their own, that Mixolume's code merely *talks to* at
runtime through a public, generic OS API (`AudioObjectGetPropertyData` /
`AudioObjectSetPropertyData`, part of Core Audio's public HAL client interface, not part of
BackgroundMusic itself). Mixolume:

- Does not vendor a single line of BackgroundMusic's source, in this directory or anywhere else
  in the repo.
- Does not fork, patch, or redistribute BGMDriver, BGMApp, or BGMXPCHelper binaries.
- Only reads the values of a small number of publicly-documented-shape HAL property selectors
  (a UID string and a handful of `FourCharCode` constants) that BackgroundMusic's driver happens
  to register on the specific device object it publishes, using APIs Apple ships as part of the
  OS. Those selector *values* (see `src/mixer/macos.rs`) were read from BackgroundMusic's public
  header (`SharedSource/BGM_Types.h`) purely to know what number to pass to Apple's own API --
  no code from that header is reproduced or linked.
- Requires the end user to separately download/install BackgroundMusic from its own releases,
  under its own GPLv2 license, exactly as they would install any other Core Audio HAL plugin.

This is the same relationship many non-GPL applications have with GPL command-line tools they
shell out to (e.g. calling a user-installed `ffmpeg`), which is broadly understood in the open
source community as mere aggregation, not linking/derivation. **This document does not
constitute legal advice**; if Mixolume moves toward a real release, this reasoning should get a
pass from an actual lawyer.

## Open product/legal question this file deliberately leaves unresolved

BackgroundMusic is two cooperating programs, both GPLv2:

- **BGMDriver** -- the Core Audio HAL plugin (virtual device). This is the piece
  `src/mixer/macos.rs` talks to.
- **BGMApp** -- a separate, ordinary macOS host app that (a) sets BGMDriver's virtual device as
  the system default output, (b) plays the mixed audio through to the real output device, and
  (c) is, in the current BackgroundMusic architecture, the thing that actually sends per-app
  volume changes to BGMDriver via the `kAudioDeviceCustomPropertyAppVolumes` property.

**Open question, not resolved here:** does Mixolume need the user to *also* separately install
and keep running an independent, unmodified BGMApp (as its own system utility, playing the same
"mere aggregation" role as BGMDriver) for routing + playback-through to work at all -- or can
Mixolume's own Rust/Tauri code fully replace BGMApp's role by:

1. Setting BGMDriver's virtual device as the system default output itself (via
   `kAudioHardwarePropertyDefaultOutputDevice`), and
2. Running its own real-time tap that reads from BGMDriver's device and writes to the real
   output device (the "play the mix through" part BGMApp currently does)?

`src/mixer/macos.rs` only implements step 3's *sibling* concern -- reading/writing per-app volume
on the already-published `kAudioDeviceCustomPropertyAppVolumes` property, which is mechanism-wise
identical regardless of who else is running. It does **not** implement default-device switching
or the audio-passthrough tap, and does not assume BGMApp is or isn't running. **This is a real
open decision for the maintainer** (it has both a legal dimension -- writing a passthrough
tap/host is squarely "new code Mixolume owns," fine under mere aggregation, so long as it doesn't
embed BGMApp's own source -- and a product dimension -- asking users to install two separate
utilities is friction). Do not silently assume either answer in future work on this backend
without flagging it explicitly.

## Development environment requirements (not provided by this repo)

Building, installing, or testing BGMDriver itself requires a **real Mac and Xcode** -- neither
of which exist in this repository's toolchain or CI. This repo does not, and will not, vendor
BackgroundMusic's Xcode project, its build scripts, or its compiled driver bundle. Everything in
`src/mixer/macos.rs` was written without the ability to compile it (see that file's top-of-file
doc comment for the specific spots most likely to need adjustment on first real build).

### What a contributor with a Mac needs to do

This list is concrete but **unverified end-to-end** -- follow BackgroundMusic's own
documentation as the source of truth if anything here is stale:

1. Install BackgroundMusic as an end user would, from its own releases (e.g. via
   `brew install background-music` or a signed installer/`.pkg` from
   <https://github.com/kyleneideck/BackgroundMusic/releases>) -- **not** by cloning and building
   it inside this repo's tree.
2. Confirm the driver loaded: open `Audio MIDI Setup.app` (or run `system_profiler SPAudioDataType`
   / enumerate devices via `AudioObjectGetPropertyData(kAudioHardwarePropertyDevices, ...)`) and
   look for a device whose UID (`kAudioDevicePropertyDeviceUID`) is exactly `"BGMDevice"`. If it's
   not there, `src/mixer/macos.rs`'s `find_bgm_device()` will return a `MixerError::Platform`
   explaining the same thing -- that is the expected, handled failure mode for "not installed."
3. Build Mixolume's Rust code (`cargo build` inside `src-tauri/`) and fix whatever doesn't compile
   in `mixer/macos.rs` first -- see that file's top comment for the specific API surfaces flagged
   as unverified (`CFDictionary::find`'s exact signature in the pinned `core-foundation` version,
   and whether `kAudioObjectPropertyElementMaster` vs. `kAudioObjectPropertyElementMain` is what
   the pinned `coreaudio-sys` version's bindgen output actually named it).
4. With some app (e.g. a browser tab playing audio, or `afplay`) producing sound, call
   `MacosMixerBackend::list_sessions()` and confirm it returns a non-empty list once BGMDriver is
   also the active output device -- this requires BGMDriver to actually be *in* the audio path
   (i.e. something -- today, BGMApp -- has set it as the default output and is passing audio
   through), which ties back to the open question above. Until that routing question is
   resolved, expect `list_sessions()` to legitimately return an empty list even with BGMDriver
   installed, if nothing has made it the active output device yet.
5. Exercise `set_volume` / `set_muted` against a real session id from step 4 and confirm the
   target app's audible volume actually changes.

Please update this file with what you actually observe once you've done this -- it will very
likely disagree with something above in the details.
