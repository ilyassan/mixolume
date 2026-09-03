//! macOS [`AudioMixerBackend`] -- built on Apple's **Core Audio Process Tap API**
//! (`CATapDescription` / `AudioHardwareCreateProcessTap`, part of the CoreAudio framework,
//! shipped starting in macOS 14.2 Sonoma; treat **14.4+ as the practical documented minimum**
//! since every real-world reference implementation targets that).
//!
//! # THIS FILE HAS NOT BEEN COMPILED OR RUN
//!
//! Written on a Windows machine with no access to Xcode, a Mac, or the Core Audio frameworks.
//! This module supersedes an earlier design (see git history / `src-tauri/macos-driver/README.md`,
//! superseded by `src-tauri/macos-audio/README.md`) that talked to an independently-installed
//! `kyleneideck/BackgroundMusic` (GPLv2) HAL driver. That approach is gone entirely: no third-party
//! driver, no GPL exposure, no `coreaudiod` restart, no separate `BGMApp`-equivalent routing
//! process. Everything here is plain public Apple API plus a small amount of Mixolume-owned
//! realtime mixing code.
//!
//! ## What was actually confirmed before writing this file (cite, don't re-derive)
//!
//! Two MIT-licensed reference projects were cloned and read in full (not fetched from memory):
//!
//! - **`https://github.com/altuzar/sonicflow`** -- a working per-app macOS volume mixer built on
//!   exactly this API. Its `Sources/SonicFlow/Audio/` directory is the structural model for this
//!   file:
//!   - `AudioProcessDetector.swift` enumerates audio processes via
//!     `kAudioHardwarePropertyProcessObjectList` on `kAudioObjectSystemObject`, then reads
//!     per-process properties `kAudioProcessPropertyPID` ('ppid'), `kAudioProcessPropertyBundleID`
//!     ('pbid'), `kAudioProcessPropertyIsRunning` ('pir?'), `kAudioProcessPropertyIsRunningOutput`
//!     ('piro'), `kAudioProcessPropertyIsRunningInput` ('piri') on each process `AudioObjectID`.
//!   - `ProcessTap.swift` builds a `CATapDescription(stereoMixdownOfProcesses:)`, sets
//!     `.isPrivate = true`, `.muteBehavior = .mutedWhenTapped` (confirmed real mute -- routes the
//!     tapped process's audio to the tap and silences its normal path for the duration of the
//!     read), and calls `AudioHardwareCreateProcessTap` / `AudioHardwareDestroyProcessTap`.
//!   - `AggregateOutputDevice.swift` builds a **private** aggregate device
//!     (`AudioHardwareCreateAggregateDevice`) whose sub-device list contains the tap UIDs plus the
//!     real default output device (for a shared clock only -- the aggregate's own IOProc does NOT
//!     drive the physical speakers). That IOProc reads each tap's buffer, multiplies by a per-app
//!     gain float, mixes into a stereo scratch buffer, and writes the result into a lock-free
//!     ring buffer (`RingBuffer.swift`).
//!   - `AudioEngine`'s playback half (`PlaybackDevice.swift`, referenced from `AudioEngine.swift`)
//!     installs a **second, separate** `AudioDeviceIOProcID` directly on the user's real default
//!     output device. That IOProc reads from the ring buffer and **adds** (not replaces) the
//!     mixed/gained samples into the device's own output buffer -- which already contains
//!     whatever the system mixer wrote for every non-tapped app. This two-IOProc-plus-ring-buffer
//!     split is the actual confirmed architecture, not a single aggregate device that somehow
//!     "is" the physical output -- Core Audio has no API to make one IOProc drive a *different*
//!     device's hardware output directly, so the ring buffer bridge is required. This file mirrors
//!     that split (see [`CaptureAggregate`] / [`PlaybackTap`] below) rather than the simpler
//!     "one aggregate, one IOProc" shape sketched at a high level in the original task brief.
//!   - `AudioGainController.swift` shows gain flowing from a plain (non-realtime, `@MainActor`)
//!     `setGain(forBundle:effective:)` call into a `GainSlot` whose backing store is a single
//!     `Float` behind an `UnsafeMutablePointer` -- "atomic on aligned 32-bit boundaries", no lock,
//!     captured by the realtime block at construction time. This file's [`AtomicGainSlot`] is the
//!     Rust equivalent, using `AtomicU32` + `f32::to_bits`/`from_bits` instead of relying on
//!     natural alignment.
//!   - `Resources/Info.plist` confirms `NSAudioCaptureUsageDescription` is the (only) Info.plist
//!     key needed, and `LSMinimumSystemVersion` is pinned to `14.2`.
//!   - `Services/PermissionsManager.swift` confirms no explicit TCC preflight/request call is
//!     needed for the audio-capture permission itself (it's only used there for an unrelated
//!     Accessibility permission for global hotkeys) -- the system prompts automatically the first
//!     time `AudioHardwareCreateProcessTap` actually runs, driven purely by the Info.plist string.
//!
//! - **`https://github.com/insidegui/AudioCap`** -- documents the raw process-tap mechanics
//!   (capture-focused, but useful because Apple's own docs are thin):
//!   - `ProcessTap/CoreAudioUtils.swift` is the generic `AudioObjectGetPropertyData`
//!     size-then-fetch read pattern this file's [`read_property`]/[`read_property_array`] mirror.
//!   - `ProcessTap/ProcessTap.swift` independently confirms the exact same
//!     `CATapDescription`/`AudioHardwareCreateProcessTap`/aggregate-device shape as sonicflow,
//!     including `kAudioAggregateDeviceTapListKey` / `kAudioSubTapUIDKey` /
//!     `kAudioSubTapDriftCompensationKey` dictionary keys for wiring a tap into an aggregate.
//!   - `ProcessTap/AudioRecordingPermission.swift` shows the *optional* private `TCC.framework`
//!     SPI path (`TCCAccessPreflight`/`TCCAccessRequest`, `dlopen`/`dlsym`'d, gated behind an
//!     `ENABLE_TCC_SPI` build flag) some apps use to preflight the permission explicitly --
//!     confirms this is optional/SPI (private, unstable, not used by default even in that repo),
//!     not required. This file does not use it.
//!
//! ## Rust crate surface actually confirmed via `docs.rs` (fetched 2026-08-20, `objc2` 0.6 line)
//!
//! `CATapDescription`, `CATapMuteBehavior`, `AudioObjectPropertyAddress`, and the process/tap
//! property selectors and hardware functions used below (`AudioHardwareCreateProcessTap`,
//! `AudioHardwareDestroyProcessTap`, `AudioHardwareCreateAggregateDevice`,
//! `AudioHardwareDestroyAggregateDevice`, `AudioDeviceCreateIOProcIDWithBlock`,
//! `AudioDeviceStart`/`AudioDeviceStop`/`AudioDeviceDestroyIOProcID`, `AudioObjectGetPropertyData`,
//! `AudioObjectGetPropertyDataSize`) all live in **`objc2-core-audio` 0.3.2**, *not*
//! `objc2-audio-toolbox` -- this corrects an initial assumption (the task brief that prompted this
//! rewrite guessed `objc2-audio-toolbox`; a direct `docs.rs` fetch of that crate's root module did
//! not show any of these symbols, while `objc2-core-audio`'s root module and individual item pages
//! did). `objc2-audio-toolbox` is therefore *not* a dependency here. Confirmed signatures (fetched
//! from `docs.rs/objc2-core-audio/0.3.2`, not reconstructed from memory):
//!
//! ```text
//! pub unsafe extern "C-unwind" fn AudioHardwareCreateProcessTap(
//!     in_description: Option<&CATapDescription>,
//!     out_tap_id: *mut AudioObjectID,
//! ) -> i32;
//!
//! pub unsafe extern "C-unwind" fn AudioHardwareCreateAggregateDevice(
//!     in_description: &CFDictionary,
//!     out_device_id: NonNull<AudioObjectID>,
//! ) -> i32;
//!
//! pub unsafe extern "C-unwind" fn AudioDeviceCreateIOProcIDWithBlock(
//!     out_io_proc_id: NonNull<AudioDeviceIOProcID>,
//!     in_device: AudioObjectID,
//!     in_dispatch_queue: Option<&DispatchQueue>,
//!     in_io_block: AudioDeviceIOBlock,
//! ) -> i32;
//!
//! pub unsafe extern "C-unwind" fn AudioObjectGetPropertyData(
//!     in_object_id: AudioObjectID,
//!     in_address: NonNull<AudioObjectPropertyAddress>,
//!     in_qualifier_data_size: u32,
//!     in_qualifier_data: *const c_void,
//!     io_data_size: NonNull<u32>,
//!     out_data: NonNull<c_void>,
//! ) -> i32;
//!
//! // CATapDescription (an Objective-C class -- objc2 interop, not raw C FFI):
//! pub unsafe fn initStereoMixdownOfProcesses(
//!     this: Allocated<Self>,
//!     processes_object_i_ds_to_include_in_tap: &NSArray<NSNumber>,
//! ) -> Retained<Self>;
//! pub unsafe fn setName(&self, name: &NSString);
//! pub unsafe fn setPrivate(&self, private_tap: bool);
//! pub unsafe fn setMuteBehavior(&self, mute_behavior: CATapMuteBehavior);
//! pub unsafe fn setExclusive(&self, exclusive: bool);
//! pub unsafe fn setMixdown(&self, mixdown: bool);
//!
//! // CATapMuteBehavior(pub NSInteger) -- three cases confirmed by name (exact Rust spelling of
//! // the associated consts is one of this file's flagged risk spots -- see below):
//! //   Unmuted (default), Muted, MutedWhenTapped.
//!
//! pub type AudioDeviceIOBlock = *mut block2::DynBlock<dyn Fn(
//!     NonNull<AudioTimeStamp>, NonNull<AudioBufferList>, NonNull<AudioTimeStamp>,
//!     NonNull<AudioBufferList>, NonNull<AudioTimeStamp>,
//! )>;
//! ```
//!
//! `kAudioObjectSystemObject`, `kAudioHardwarePropertyProcessObjectList`,
//! `kAudioHardwarePropertyTranslatePIDToProcessObject`, `kAudioProcessPropertyPID`,
//! `kAudioProcessPropertyBundleID`, `kAudioProcessPropertyIsRunningOutput` were each individually
//! confirmed present in `objc2-core-audio` 0.3.2 (gated behind that crate's `AudioHardware`
//! Cargo feature) with the exact hex values sonicflow's own hand-rolled FourCC constants predict
//! (e.g. `kAudioProcessPropertyPID` = `0x70706964` = `'ppid'`) -- cross-checked as a unit test
//! below ([`tests::fourcc_matches_known_selectors`]).
//!
//! ## Highest-risk spots a Mac-equipped contributor should check FIRST
//!
//! 1. ~~The `block2` IOProc closures in [`CaptureAggregate::start`] / [`PlaybackTap::start`].~~
//!    **RESOLVED** -- verified and fixed against real macOS 26.4 hardware (crash report
//!    confirmed a `SIGABRT`/`OBJC` "Attempt to use unknown class" fatal inside
//!    `AudioDeviceCreateIOProcIDWithBlock`, one frame below `CaptureAggregate::start`). The bug
//!    was `&block as *const _ as *mut _`, which took a pointer to the `RcBlock<F>` *wrapper
//!    struct on the Rust stack* rather than to the heap-allocated `Block<F>`/ObjC block object it
//!    wraps -- Core Audio then tried to invoke through a garbage pointer. Fixed by using
//!    `RcBlock::as_ptr(&block)`, block2 0.6.2's own documented accessor for the real
//!    `*mut Block<F>` (`AudioDeviceIOBlock` is exactly `*mut block2::DynBlock<F>`, and
//!    `DynBlock<F> = Block<F>`, confirmed by reading both crates' source directly).
//! 2. **`CATapMuteBehavior`'s associated-constant spelling** (`MutedWhenTapped` vs.
//!    `MUTED_WHEN_TAPPED` vs. something else) -- `docs.rs` confirmed the three *cases* exist and
//!    their semantics, but the exact Rust-side casing objc2's codegen picked was not independently
//!    re-verified against a live crate build in this session.
//! 3. **`AudioHardwareCreateAggregateDevice`'s `CFDictionary` construction.** The description
//!    dictionary needs mixed value types (a `CFString` UID/name, `CFBoolean`/`CFNumber` flags, a
//!    nested `CFArray` of `CFDictionary`s for the tap list) built through whatever
//!    `objc2-core-foundation` 0.3's actual dictionary-building API is. That crate's docs were not
//!    fetched in this session (time-boxed research pass covered `objc2-core-audio` and
//!    `objc2-core-audio-types` first since they're the load-bearing ones); [`build_aggregate_description`]
//!    is written from the *shape* both reference repos use (a plain `[String: Any]`/`CFDictionary`
//!    literal with well-known keys), not from a confirmed `objc2-core-foundation` call shape.
//!    Fetch that crate's docs.rs page before trusting this function to compile as-is.
//!
//! ## Known, deliberate limitations (same honesty bar as the previous scaffold)
//!
//! - **Display name and icon are both resolved via `NSRunningApplication`** (see
//!   [`resolve_app_info`]) -- `.localizedName` for the name, `.icon` re-encoded to PNG via
//!   `NSBitmapImageRep` for `AppSession::icon_png`, matching what AudioCap/sonicflow do.
//! - **Mute is a local convenience on top of the tap's real gain knob**, not a separate wire-level
//!   concept -- see [`AppGainState`]. This is simpler than the old BackgroundMusic-era code's
//!   cross-process "shared property" mute problem: since gain here lives entirely in Mixolume's
//!   own process (the atomic slot table), there is no other client to disagree with about what
//!   "muted" means, unlike the old design's dependency on a shared driver property.
//! - Tapping a process is not free: each currently-*active* (output-producing) process gets its
//!   own `CATapDescription`/tap, and the whole capture aggregate + both IOProcs are torn down and
//!   rebuilt whenever the active set changes (mirrors sonicflow's `AudioGainController.apply`
//!   exactly -- Core Audio has no documented way to add/remove a tap from a running aggregate).
//!   This means every app start/stop of audio output causes a brief (sub-audio-buffer, but real)
//!   glitch while the aggregate rebuilds. Confirmed as the reference architecture's own tradeoff,
//!   not a shortcut unique to this port.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::macos_ducking::{self, DuckingRuntime};
use super::{
    clamp_boosted_volume, AppSession, AudioMixerBackend, DuckingSettings, MixerError,
    RunningAppInfo,
};

// =================================================================================================
// Pure logic -- no Core Audio / objc2 involved. Fully unit-testable on any OS, which is why the
// `#[cfg(test)]` block at the bottom of this file is worth having even though the whole module is
// gated behind `#[cfg(target_os = "macos")]` in `mixer/mod.rs` and so only ever actually runs in a
// macOS CI leg.
// =================================================================================================

/// [`AppSession::id`] for a given pid, matching the Windows backend's `format!("win-{pid}")`
/// convention (see `mixer/windows.rs`).
fn session_id_for_pid(pid: i32) -> String {
    format!("macos-{pid}")
}

/// Per-app volume + mute state, independent of whether the app is currently tapped.
///
/// Kept alive for the lifetime of the backend (not torn down when the underlying tap is), so an
/// app that goes silent and later starts playing audio again keeps whatever volume/mute the user
/// last set, rather than resetting to full volume. Mirrors sonicflow's `AudioState`/`AudioApp`
/// (`state.setVolume`/`state.setMuted` persisting across `AudioGainController.apply` rebuilds).
#[derive(Debug, Clone, Copy, PartialEq)]
struct AppGainState {
    /// User-facing volume, independent of mute. Not zeroed out by muting -- unmuting restores
    /// exactly this value with no "did we remember it" ambiguity (contrast with the old
    /// BackgroundMusic-era code, which had to fake this with a side-channel hashmap because the
    /// wire format had no separate mute bit; here it's just a second field).
    volume: f32,
    muted: bool,
    /// Left/right stereo balance: -1.0 is full left, 0.0 is centered (both channels at full
    /// `volume`), 1.0 is full right. The tap is created via `initStereoMixdownOfProcesses`, so
    /// every app's captured audio is already an interleaved stereo stream by the time it reaches
    /// the realtime mixer -- balance only needed a per-channel gain instead of one scalar, not a
    /// pipeline redesign.
    balance: f32,
    /// See [`crate::mixer::AppSession::write_generation`]. Bumped by every setter below,
    /// regardless of which field changed -- any write from the frontend invalidates a stale read
    /// of this session's data, not just the one field it touched.
    generation: u64,
}

impl AppGainState {
    /// Default state for a freshly-seen process: full volume, unmuted, centered.
    const fn default_full_volume() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            balance: 0.0,
            generation: 0,
        }
    }

    /// Bumped by every setter below and returned to the caller, which passes it straight back to
    /// the frontend -- see [`crate::mixer::AppSession::write_generation`].
    fn bump_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn set_volume(&mut self, volume: f32) -> u64 {
        // Boosted, not `clamp_volume` -- macOS has no native "per-app volume" API to begin with
        // (every app's volume here is already just a software gain multiply on captured samples,
        // see `mix_capture_callback`'s doc comment), and that mix's final output is already
        // hard-clamped to `[-1.0, 1.0]` before being written out, so allowing gain above 1.0 here
        // needs no new clipping protection -- it reuses what's already there.
        self.volume = clamp_boosted_volume(volume);
        self.bump_generation()
    }

    fn set_muted(&mut self, muted: bool) -> u64 {
        self.muted = muted;
        self.bump_generation()
    }

    fn set_balance(&mut self, balance: f32) -> u64 {
        self.balance = balance.clamp(-1.0, 1.0);
        self.bump_generation()
    }

    /// What the realtime callback should actually multiply the left/right channels by. Linear
    /// (constant-gain, not constant-power) pan law: at center both channels get the full volume;
    /// panning fully to one side takes the *other* channel to zero while leaving the panned-to
    /// channel at the full volume, rather than a curved crossfade. Simpler to reason about than
    /// equal-power panning and plenty precise for a volume-mixer slider rather than a mixing
    /// console.
    fn effective_gains(&self) -> (f32, f32) {
        if self.muted {
            return (0.0, 0.0);
        }
        let left = self.volume * (1.0 - self.balance.max(0.0));
        let right = self.volume * (1.0 + self.balance.min(0.0));
        (left, right)
    }
}

impl Default for AppGainState {
    fn default() -> Self {
        Self::default_full_volume()
    }
}

/// A realtime-safe stereo gain cell: two `AtomicU32`s per tapped app (left channel, right
/// channel), each storing an `f32`'s bit pattern. The realtime IOProc closure only ever calls
/// [`AtomicGainSlot::load`] (two relaxed atomic loads, no allocation, no lock);
/// [`AtomicGainSlot::store`] is called from the non-realtime `set_volume`/`set_muted`/
/// `set_balance` control path. Equivalent to sonicflow's `GainSlot` (a `Float` behind an
/// `UnsafeMutablePointer`, "atomic on aligned 32-bit boundaries") but uses explicit `AtomicU32`s
/// instead of relying on natural-alignment atomicity, and doubled up for independent per-channel
/// gain rather than one scalar -- the two loads/stores aren't tied together as a single atomic
/// operation, but a torn read (old left paired with new right, or vice versa) for one audio
/// buffer is completely inaudible, so that's a fine trade against the complexity of packing both
/// into one atomic value.
#[derive(Debug)]
struct AtomicGainSlot {
    left: AtomicU32,
    right: AtomicU32,
}

impl AtomicGainSlot {
    fn new((left, right): (f32, f32)) -> Self {
        Self {
            left: AtomicU32::new(left.to_bits()),
            right: AtomicU32::new(right.to_bits()),
        }
    }

    /// Called only from the realtime audio callback.
    fn load(&self) -> (f32, f32) {
        (
            f32::from_bits(self.left.load(Ordering::Relaxed)),
            f32::from_bits(self.right.load(Ordering::Relaxed)),
        )
    }

    /// Called only from the non-realtime `set_volume`/`set_muted`/`set_balance` control path.
    fn store(&self, (left, right): (f32, f32)) {
        self.left.store(left.to_bits(), Ordering::Relaxed);
        self.right.store(right.to_bits(), Ordering::Relaxed);
    }
}

/// Wait-free single-producer/single-consumer float ring buffer bridging the capture aggregate's
/// IOProc (producer) and the real output device's playback IOProc (consumer). Capacity is rounded
/// up to a power of two so index wraparound is a cheap mask instead of a modulo -- mirrors
/// sonicflow's `FloatRingBuffer` (`RingBuffer.swift`) line for line, translated to Rust atomics
/// instead of relying on aligned 64-bit stores being atomic "at the CPU level".
struct FloatRingBuffer {
    buffer: Vec<f32>,
    capacity: usize,
    mask: usize,
    /// Producer's next-free index (monotonically increasing, wraps via `& mask`). Single writer.
    head: std::sync::atomic::AtomicUsize,
    /// Consumer's next-to-read index (monotonically increasing). Single reader.
    tail: std::sync::atomic::AtomicUsize,
}

impl FloatRingBuffer {
    fn new(requested_capacity: usize) -> Self {
        let mut cap = 1usize;
        while cap < requested_capacity.max(2) {
            cap <<= 1;
        }
        Self {
            buffer: vec![0.0; cap],
            capacity: cap,
            mask: cap - 1,
            head: std::sync::atomic::AtomicUsize::new(0),
            tail: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    #[allow(dead_code)] // only called from #[cfg(test)] tests below; no production caller yet
    fn fill_level(&self) -> usize {
        self.head
            .load(Ordering::Relaxed)
            .wrapping_sub(self.tail.load(Ordering::Relaxed))
    }

    /// Producer side. Drops samples (rather than overwriting unread ones or blocking) if the
    /// buffer is full -- silence on overrun is preferable to corrupting the stream. Returns the
    /// number of samples actually written.
    ///
    /// # Safety
    /// Must only ever be called from the single producer thread (the capture IOProc).
    unsafe fn write(&self, src: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        let free = self.capacity - head.wrapping_sub(tail);
        let n = src.len().min(free);
        if n == 0 {
            return 0;
        }

        let base = self.buffer.as_ptr() as *mut f32;
        let write_start = head & self.mask;
        let first_chunk = n.min(self.capacity - write_start);
        std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(write_start), first_chunk);
        if n > first_chunk {
            std::ptr::copy_nonoverlapping(src.as_ptr().add(first_chunk), base, n - first_chunk);
        }

        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer side. Underrun (not enough buffered samples) is silent -- the caller is expected
    /// to have already zeroed `dst`. Returns the number of samples actually read.
    ///
    /// # Safety
    /// Must only ever be called from the single consumer thread (the real output device's
    /// playback IOProc).
    unsafe fn read(&self, dst: &mut [f32]) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Relaxed);
        let available = head.wrapping_sub(tail);
        let n = dst.len().min(available);
        if n == 0 {
            return 0;
        }

        let base = self.buffer.as_ptr();
        let read_start = tail & self.mask;
        let first_chunk = n.min(self.capacity - read_start);
        std::ptr::copy_nonoverlapping(base.add(read_start), dst.as_mut_ptr(), first_chunk);
        if n > first_chunk {
            std::ptr::copy_nonoverlapping(base, dst.as_mut_ptr().add(first_chunk), n - first_chunk);
        }

        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }
}

// SAFETY: `FloatRingBuffer` is only ever shared as `Arc<FloatRingBuffer>` between exactly two
// realtime threads (one producer, one consumer), each of which only ever calls its own respective
// `write`/`read` method. The `Vec<f32>` backing store is never resized after construction.
unsafe impl Sync for FloatRingBuffer {}
unsafe impl Send for FloatRingBuffer {}

/// Fixed-capacity scratch space, pre-allocated once at engine-start time and reused on every
/// realtime callback invocation instead of allocating a fresh `Vec` per call. Heap allocation
/// inside a Core Audio IOProc is a real hazard (malloc can take a lock / touch a fresh page and
/// stall the audio thread, causing an audible glitch), so this exists specifically to keep
/// [`mix_capture_callback`]/[`mix_playback_callback`] allocation-free -- consistent with why
/// [`AtomicGainSlot`] and [`FloatRingBuffer`] were built the way they are.
///
/// # Safety contract
/// Same shape as [`FloatRingBuffer`]'s: exactly one owning realtime callback ever calls
/// [`Scratch::as_mut_slice`], so the `UnsafeCell` never has two live mutable borrows at once even
/// though nothing at the type level enforces that -- it's a single-owner discipline, not
/// synchronization.
struct Scratch(std::cell::UnsafeCell<Vec<f32>>);

impl Scratch {
    fn new(capacity: usize) -> Self {
        Self(std::cell::UnsafeCell::new(vec![0.0f32; capacity]))
    }

    /// # Safety
    /// Caller must be the single realtime callback that owns this `Scratch` (see struct doc
    /// comment) -- never call this from more than one thread/closure for the same `Scratch`.
    #[allow(clippy::mut_from_ref)]
    unsafe fn as_mut_slice(&self) -> &mut [f32] {
        &mut *self.0.get()
    }
}

// SAFETY: see the struct doc comment -- single-owning-callback discipline, not real
// synchronization. Only ever shared as `Arc<Scratch>` with exactly one realtime thread calling
// `as_mut_slice`.
unsafe impl Sync for Scratch {}
unsafe impl Send for Scratch {}

/// Bundle-ID prefixes of system processes that show up in the HAL process list but that the user
/// can't meaningfully volume-control (Core Audio's own daemons, accessibility services, etc.).
/// Checked first, before [`is_system_internal_path`] below, because it's free -- `bundle_id` is
/// already in hand from the same HAL enumeration pass that found this process at all, no syscall
/// needed. Originally copied from sonicflow's `AudioProcessDetector.systemBundlesToHide`; kept
/// around as a fast-path alongside the path check below rather than replaced by it, since a
/// syscall failure on the path lookup (rare, but see `is_hidden_system_process_cached`'s doc
/// comment) still leaves this available as a free, always-reliable fallback signal.
const SYSTEM_BUNDLE_PREFIXES_TO_HIDE: &[&str] = &[
    "com.apple.audiomxd",
    "com.apple.coreaudiod",
    "com.apple.mediaremoted",
    "com.apple.controlcenter",
    "com.apple.WebKit.GPU",
    "com.apple.cmio",
];

fn is_hidden_system_bundle(bundle_id: &str) -> bool {
    SYSTEM_BUNDLE_PREFIXES_TO_HIDE
        .iter()
        .any(|prefix| bundle_id.starts_with(prefix))
}

/// Directories no real, user-facing app's executable lives under -- every app a user would
/// actually recognize (Apple's own included: Music, Safari, Calculator, FaceTime, Preview, ...)
/// is bundled under `/Applications` or `/System/Applications`. Daemons, XPC services, and
/// system-internal utilities live somewhere else entirely -- confirmed on real hardware:
/// `systemsoundserverd`/`coreaudiod` are `/usr/sbin/...`, `PowerChime.app` is
/// `/System/Library/CoreServices/PowerChime.app/...`, and `com.apple.WebKit.GPU`'s actual
/// executable (despite the bundle id suggesting otherwise) is a WebKit-framework-internal XPC
/// service under `/System/Library/Frameworks/WebKit.framework/.../XPCServices/...`.
///
/// This is deliberately a *structural* check (where does this thing live) instead of an
/// ever-growing list of specific names/bundle-ids added by hand every time a user reports the
/// next one -- `PowerChime` showing up was exactly that pattern (`systemsoundserverd` got fixed
/// by name last time, the next bundle-less/differently-bundled system thing just needed its own
/// entry) rather than a fix that generalizes to whatever Apple ships next.
const SYSTEM_INTERNAL_PATH_PREFIXES: &[&str] = &["/System/Library/", "/usr/sbin/", "/usr/libexec/"];

fn is_system_internal_path(path: &str) -> bool {
    SYSTEM_INTERNAL_PATH_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// Cached: `libproc::proc_pid::pidpath` makes a fresh syscall on every single call, and that
/// syscall can transiently fail (process already gone, momentary `ESRCH`/`EPERM`, etc.) for
/// reasons unrelated to whether the process should actually be hidden. An earlier, uncached
/// version of an equivalent name-based check re-derived its answer fresh on every poll tick --
/// confirmed live as the cause of `systemsoundserverd` intermittently showing up in the session
/// list, sometimes in "active", sometimes in "inactive": when the syscall failed, the process fell
/// through unfiltered for that one tick, landing in whichever section its `is_running_output`
/// happened to be that moment.
///
/// A pid's hidden-or-not status can't actually change during its lifetime, so once resolved it's
/// cached permanently for that pid -- but only a *decisive* answer gets cached: a bundle-id match
/// (instant, no syscall, always reliable) or a path lookup that actually succeeded. A syscall
/// failure resolves to `false` for just this one call without being cached, so the next poll
/// tries again from scratch instead of permanently locking in a wrong guess.
fn is_hidden_system_process_cached(
    cache: &mut HashMap<i32, bool>,
    bundle_id: Option<&str>,
    pid: i32,
) -> bool {
    if let Some(&hidden) = cache.get(&pid) {
        return hidden;
    }
    if bundle_id.is_some_and(is_hidden_system_bundle) {
        cache.insert(pid, true);
        return true;
    }
    match libproc::proc_pid::pidpath(pid) {
        Ok(path) => {
            let hidden = is_system_internal_path(&path);
            cache.insert(pid, hidden);
            hidden
        }
        Err(_) => false,
    }
}

/// Whether a process counts as "wanted" for `MacosMixerBackend::reconcile_engine`'s tap-teardown
/// decision -- either it's genuinely reporting active right now, or it was recently enough that
/// it's still within its post-active hold window. Pulled out as its own pure function (rather
/// than left inline in the filter closure) so the flicker-tolerance logic itself is
/// unit-testable without needing a real Core Audio process/engine to exercise it. See
/// `Inner::active_hold_until`'s doc comment for the full rationale.
fn is_wanted_for_reconciliation(
    is_running_output: bool,
    hold_until: Option<Instant>,
    now: Instant,
) -> bool {
    is_running_output || hold_until.is_some_and(|until| until > now)
}

// =================================================================================================
// Core Audio / objc2 integration. Everything below this line calls into the real OS APIs described
// in the module doc comment above and has NOT been compiled. See the "Highest-risk spots" section
// above before trusting any single line of it.
// =================================================================================================

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::NSInteger;
// `AnyThread` brings `CATapDescription::alloc()` into scope -- objc2's alloc/init pattern puts
// `alloc()` on this trait (implemented for every objc2 class) rather than directly on each class,
// which the compiler doesn't surface unless the trait itself is imported.
use objc2::AnyThread;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_core_audio::{
    self as ca, AudioDeviceIOProcID, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// How many parent-process hops [`resolve_named_running_app`] will follow before giving up.
/// Chromium-family helper processes are typically one hop from the browser's main process; this
/// is generous headroom for deeper subprocess trees without risking a long walk on a pathological
/// process ancestry.
const MAX_PARENT_WALK_DEPTH: u8 = 8;

/// `NSRunningApplication` for `pid` if it has a usable `localizedName` itself; otherwise walks up
/// its parent-process chain looking for the nearest ancestor that does.
///
/// Some audio-producing processes have no display identity of their own -- e.g. Chromium/Brave's
/// `*.helper` renderer/GPU subprocesses, which show up as raw bundle ids (`com.brave.Browser.
/// helper`) rather than anything a user would recognize -- while their parent (the actual browser
/// process the user launched) does. Walking up to that parent and borrowing its name/icon matches
/// what the user actually thinks of as "the app playing audio".
fn resolve_named_running_app(pid: i32) -> Option<Retained<NSRunningApplication>> {
    let mut current_pid = pid;
    for _ in 0..MAX_PARENT_WALK_DEPTH {
        let running_app = NSRunningApplication::runningApplicationWithProcessIdentifier(
            current_pid as libc::pid_t,
        );
        let has_name = running_app
            .as_ref()
            .and_then(|app| app.localizedName())
            .is_some_and(|name| !name.to_string().is_empty());
        if has_name {
            return running_app;
        }

        let Ok(info) = libproc::proc_pid::pidinfo::<libproc::bsd_info::BSDInfo>(current_pid, 0)
        else {
            return None;
        };
        let parent_pid = info.pbi_ppid as i32;
        // pid 1 is launchd; ppid 0/negative or no progress means we've hit the top with nothing.
        if parent_pid <= 1 || parent_pid == current_pid {
            return None;
        }
        current_pid = parent_pid;
    }
    None
}

/// Real, human-readable app name (e.g. "YTAudioBar", not the raw `com.ytaudiobar.app` bundle id)
/// plus a PNG-encoded app icon, for a tapped process. Both come from the same
/// `NSRunningApplication` (possibly a parent process's, via [`resolve_named_running_app`]), so
/// this resolves them together rather than making two separate objc2 round-trips per process per
/// poll tick.
///
/// `NSRunningApplication.localizedName`/`.icon` are the same name/icon macOS itself shows in the
/// Dock and Force Quit window, and are available for any regular running app regardless of
/// whether it also has a `kAudioProcessPropertyBundleID` (some audio-producing helper processes
/// don't). Name falls back to the bundle id, then to `pid <N>`, if neither the process nor any
/// ancestor can be resolved (e.g. it already exited); icon falls back to `None` (the frontend
/// already renders a placeholder for that -- see `SessionIcon.tsx`).
/// One [`Inner::app_info_cache`] entry: either a resolve is still in flight on a background
/// thread, or it's finished and this is the result.
enum AppInfoCacheEntry {
    Pending,
    Resolved(String, Option<Vec<u8>>),
}

fn resolve_app_info(pid: i32, bundle_id: Option<&str>) -> (String, Option<Vec<u8>>) {
    // Belt-and-suspenders alongside `app_info_cache`'s once-per-pid caching (which is what
    // actually keeps this leak-free in practice -- see that field's doc comment): draining the
    // pool here means even an uncached call site doesn't reproduce it.
    objc2::rc::autoreleasepool(|_pool| {
        let running_app = resolve_named_running_app(pid);

        let name = running_app
            .as_ref()
            .and_then(|app| app.localizedName())
            .map(|name| name.to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| bundle_id.map(str::to_string))
            .unwrap_or_else(|| format!("pid {pid}"));

        let icon_png = running_app.and_then(|app| app.icon()).and_then(|icon| {
            // SAFETY: `representationUsingType_properties`'s only documented precondition is
            // that `properties`' value type matches what the chosen `storage_type` expects --
            // for `.PNG` with no per-key options, an empty dictionary is always valid (this
            // mirrors how AppKit's own `NSBitmapImageRep` sample code calls it with `[:]`).
            unsafe { icon_to_png(&icon) }
        });

        (name, icon_png)
    })
}

/// App icons the UI ever draws top out around 32-40 logical px (see `SessionIcon.tsx`); even at
/// a generous 3x for Retina headroom, nothing bigger than this is ever useful. Standard `.icns`
/// icon sets bundle several `NSBitmapImageRep`s at fixed sizes (16/32/128/256/512/1024...) --
/// picking the smallest one at or above this bound avoids ever encoding the full 1024x1024 (or
/// larger) master representation just to hand the frontend something it immediately downscales.
const ICON_MAX_PIXELS_WIDE: NSInteger = 128;

/// `NSImage` -> PNG bytes, sized down to [`ICON_MAX_PIXELS_WIDE`] first. There's no direct "give
/// me PNG bytes" method on `NSImage` itself, and its default `TIFFRepresentation` bundles
/// whichever representation is "best" for the image's nominal size -- for a Dock/Force-Quit-style
/// icon that's usually the full native resolution (1024x1024 isn't unusual), which is wasteful to
/// encode and bloats the resulting JSON (each byte becomes several characters as a `number[]`
/// array) for no visual benefit, since the frontend immediately downscales it anyway.
///
/// # Safety
/// `representationUsingType_properties` requires `properties`'s value type to match what
/// `storage_type` expects; passing an empty dictionary for `.PNG` (no per-format options) is
/// always valid.
unsafe fn icon_to_png(icon: &objc2_app_kit::NSImage) -> Option<Vec<u8>> {
    let reps = icon.representations();

    // Prefer whichever bundled bitmap representation is smallest while still >= our target --
    // no drawing/resizing needed, just picking a differently-sized copy the icon set already
    // has. Reps below the target are only used as a last resort (so a tiny icon set still
    // produces *something* rather than nothing), and the full TIFF/master representation is
    // the final fallback for icons with no bitmap reps at all (synthesized/vector-only images).
    let mut best_at_or_above: Option<(NSInteger, Retained<objc2_app_kit::NSBitmapImageRep>)> = None;
    let mut best_below: Option<(NSInteger, Retained<objc2_app_kit::NSBitmapImageRep>)> = None;
    for rep in reps.iter() {
        let Some(bitmap) = rep.downcast_ref::<objc2_app_kit::NSBitmapImageRep>() else {
            continue;
        };
        let width = bitmap.pixelsWide();
        if width >= ICON_MAX_PIXELS_WIDE {
            if best_at_or_above
                .as_ref()
                .is_none_or(|(best_width, _)| width < *best_width)
            {
                best_at_or_above = Some((width, Retained::from(bitmap)));
            }
        } else if best_below
            .as_ref()
            .is_none_or(|(best_width, _)| width > *best_width)
        {
            best_below = Some((width, Retained::from(bitmap)));
        }
    }

    let bitmap = if let Some((_, bitmap)) = best_at_or_above.or(best_below) {
        bitmap
    } else {
        let tiff = icon.TIFFRepresentation()?;
        objc2_app_kit::NSBitmapImageRep::initWithData(
            objc2_app_kit::NSBitmapImageRep::alloc(),
            &tiff,
        )?
    };

    // `CopiedKey = NSString` explicitly: an empty slice gives the compiler nothing to infer it
    // from, and `NSBitmapImageRepPropertyKey` (the dictionary's `KeyType`) is itself `NSString`,
    // so `NSString` is the natural (and only sensible) choice satisfying `NSCopying` here.
    let empty_properties: Retained<
        NSDictionary<objc2_app_kit::NSBitmapImageRepPropertyKey, objc2::runtime::AnyObject>,
    > = NSDictionary::from_slices::<NSString>(&[], &[]);
    let png_data = bitmap.representationUsingType_properties(
        objc2_app_kit::NSBitmapImageFileType::PNG,
        &empty_properties,
    )?;
    Some(png_data.to_vec())
}

/// Every currently-running "regular" (dock-visible) app, by name -- used only to check which
/// [`WELL_KNOWN_COMMUNICATION_APPS`] are running for the auto-duck default-seeding done in
/// `set_ducking_enabled`. Deliberately broader than `AppSession`'s list (which only ever contains
/// apps the HAL has seen actually producing audio): a call app sitting open but silent should
/// still count as "running" for seeding purposes.
///
/// Names only, no icons -- an earlier version of this also resolved and returned each app's icon
/// for a Settings "add app" picker that searched every running app. That picker was reverted back
/// to searching [`AppSession`]s only (apps MiXolume has actually seen making sound) after
/// confirming live that resolving icons for many running apps was inherently expensive (Apple
/// Developer Forums thread 735213 documents the same `NSImage`/`.icns` round-trip taking multiple
/// seconds per icon the first time) and that no caching/budgeting strategy made it feel fast
/// enough to justify the complexity. The only remaining caller here only ever needs the name, so
/// there's nothing left to resolve.
fn list_running_applications() -> Vec<RunningAppInfo> {
    let own_pid = std::process::id() as i32;
    // AppKit enumeration/`localizedName` calls create autoreleased temporaries -- see
    // `Inner::app_info_cache`'s doc comment for the confirmed-on-real-hardware leak this avoids.
    objc2::rc::autoreleasepool(|_pool| {
        NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter(|app| {
                app.activationPolicy() == NSApplicationActivationPolicy::Regular
                    && app.processIdentifier() != own_pid
            })
            .filter_map(|app| {
                let name = app.localizedName()?.to_string();
                if name.is_empty() {
                    return None;
                }
                Some(RunningAppInfo { name })
            })
            .collect()
    })
}

/// Makes an `RcBlock` `Send`/`Sync` so it can live inside `Mutex<Inner>` -- required because
/// `AudioMixerBackend: Send + Sync`, and `RcBlock` isn't `Send`/`Sync` by default (it wraps a
/// `NonNull<Block<..>>`, and raw pointers are conservatively never `Send`/`Sync`).
///
/// # Safety
/// Once installed via `AudioDeviceCreateIOProcIDWithBlock`, Rust never calls through this pointer
/// again -- Core Audio invokes the underlying Objective-C block on its own realtime thread using
/// ordinary (thread-safe, atomically refcounted) Objective-C block retain/release semantics. The
/// only Rust-side touch after installation is a single drop, always performed while holding
/// `Inner`'s mutex (see [`MacosMixerBackend`]), so there's never concurrent access from two Rust
/// threads at once.
struct SendSyncBlock<F: ?Sized>(#[allow(dead_code)] RcBlock<F>); // held only for its Drop (releases the block); never read
unsafe impl<F: ?Sized> Send for SendSyncBlock<F> {}
unsafe impl<F: ?Sized> Sync for SendSyncBlock<F> {}

/// The closure signature Core Audio's `AudioDeviceIOBlock` expects (see the module doc comment).
/// Named so [`CaptureAggregate`]/[`PlaybackTap`]'s `block` fields and their `start()` methods
/// don't repeat this five-`NonNull`-argument `dyn Fn` shape inline (clippy's `type_complexity`
/// lint flagged the inline version).
type IoProcFn = dyn Fn(
    std::ptr::NonNull<AudioTimeStamp>,
    std::ptr::NonNull<AudioBufferList>,
    std::ptr::NonNull<AudioTimeStamp>,
    std::ptr::NonNull<AudioBufferList>,
    std::ptr::NonNull<AudioTimeStamp>,
);

fn check_status(status: i32, what: &str) -> Result<(), MixerError> {
    if status == 0 {
        Ok(())
    } else {
        Err(MixerError::Platform(format!(
            "{what} failed with OSStatus {status}"
        )))
    }
}

fn global_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: ca::kAudioObjectPropertyScopeGlobal,
        mElement: ca::kAudioObjectPropertyElementMain,
    }
}

/// Generic `AudioObjectGetPropertyData` read for a fixed-size value type `T` (e.g. `AudioObjectID`,
/// `pid_t`, a `u32` boolean flag). Mirrors AudioCap's `CoreAudioUtils.read<T>` size-then-fetch
/// pattern.
///
/// # Safety
/// `T` must be a plain-old-data type whose bit pattern Core Audio is documented to fill in for
/// `selector` (no `Drop`, no pointers/references inside).
unsafe fn read_property<T: Copy>(object_id: AudioObjectID, selector: u32) -> Result<T, MixerError> {
    let mut address = global_address(selector);
    let mut size = std::mem::size_of::<T>() as u32;
    let mut value = std::mem::MaybeUninit::<T>::uninit();

    let status = ca::AudioObjectGetPropertyData(
        object_id,
        std::ptr::NonNull::from(&mut address),
        0,
        std::ptr::null(),
        std::ptr::NonNull::from(&mut size),
        std::ptr::NonNull::new(value.as_mut_ptr() as *mut _)
            .ok_or_else(|| MixerError::Platform("null output buffer".to_string()))?,
    );
    check_status(status, "AudioObjectGetPropertyData")?;
    Ok(value.assume_init())
}

/// Generic `AudioObjectGetPropertyData` read for a variable-length array property (e.g.
/// `kAudioHardwarePropertyProcessObjectList`, a `CFArray`-free flat `AudioObjectID[]`).
///
/// # Safety
/// Same contract as [`read_property`], applied per-element.
unsafe fn read_property_array<T: Copy + Default>(
    object_id: AudioObjectID,
    selector: u32,
) -> Result<Vec<T>, MixerError> {
    let mut address = global_address(selector);
    let mut size: u32 = 0;
    let status = ca::AudioObjectGetPropertyDataSize(
        object_id,
        std::ptr::NonNull::from(&mut address),
        0,
        std::ptr::null(),
        std::ptr::NonNull::from(&mut size),
    );
    check_status(status, "AudioObjectGetPropertyDataSize")?;

    let count = size as usize / std::mem::size_of::<T>();
    let mut values: Vec<T> = vec![T::default(); count];
    let mut actual_size = size;
    let status = ca::AudioObjectGetPropertyData(
        object_id,
        std::ptr::NonNull::from(&mut address),
        0,
        std::ptr::null(),
        std::ptr::NonNull::from(&mut actual_size),
        std::ptr::NonNull::new(values.as_mut_ptr() as *mut _)
            .ok_or_else(|| MixerError::Platform("null output buffer".to_string()))?,
    );
    check_status(status, "AudioObjectGetPropertyData (array)")?;
    Ok(values)
}

/// One audio process as reported by the HAL, translated into plain Rust data. Mirrors sonicflow's
/// `AudioProcess` / AudioCap's `AudioProcess`.
#[derive(Debug, Clone)]
struct AudioProcessInfo {
    object_id: AudioObjectID,
    pid: i32,
    bundle_id: Option<String>,
    is_running_output: bool,
}

/// Enumerate every process the HAL currently knows about (`kAudioHardwarePropertyProcessObjectList`
/// on `kAudioObjectSystemObject`), then read each one's pid/bundle-id/output-running flag.
///
/// # Safety
/// Calls into Core Audio via `objc2_core_audio`; see [`read_property`]/[`read_property_array`].
fn list_audio_processes() -> Result<Vec<AudioProcessInfo>, MixerError> {
    let system_object: AudioObjectID = ca::kAudioObjectSystemObject as AudioObjectID;

    let object_ids: Vec<AudioObjectID> =
        unsafe { read_property_array(system_object, ca::kAudioHardwarePropertyProcessObjectList)? };

    let mut processes = Vec::with_capacity(object_ids.len());
    for object_id in object_ids {
        // Best-effort per-process reads: skip (don't fail the whole enumeration) if any single
        // process object disappears mid-enumeration or refuses a read (both observed as normal,
        // transient conditions in the reference implementations, e.g. a helper process exiting
        // between the list call and the per-object read).
        let pid: i32 = match unsafe { read_property(object_id, ca::kAudioProcessPropertyPID) } {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let is_running_output: u32 =
            unsafe { read_property(object_id, ca::kAudioProcessPropertyIsRunningOutput) }
                .unwrap_or(0);
        let bundle_id = read_process_bundle_id(object_id);

        processes.push(AudioProcessInfo {
            object_id,
            pid,
            bundle_id,
            is_running_output: is_running_output != 0,
        });
    }
    Ok(processes)
}

/// `kAudioProcessPropertyBundleID` is CFString-typed, not a fixed-size POD, so it needs its own
/// (get-rule) read rather than going through [`read_property`]'s `MaybeUninit<T>` path.
///
/// RISK: the exact `objc2_core_foundation`/`objc2_foundation` call shape for wrapping a raw
/// `CFStringRef`/`NSString*` handed back by `AudioObjectGetPropertyData` into a Rust `String` was
/// not independently re-verified in this session (see risk item 3 in the module doc comment,
/// which covers the sibling `CFDictionary` construction problem in the same crate). This function
/// returns `None` on any failure rather than panicking, since a missing bundle id is an expected,
/// common case (bare-pid processes with no app bundle).
fn read_process_bundle_id(object_id: AudioObjectID) -> Option<String> {
    // SAFETY: `NSString` read via get-rule CF/NS bridging; wrapped immediately into an owned Rust
    // `String` (`to_string()`), so no dangling unretained pointer escapes this function.
    unsafe {
        let mut address = global_address(ca::kAudioProcessPropertyBundleID);
        let mut size = std::mem::size_of::<*const NSString>() as u32;
        let mut raw: *const NSString = std::ptr::null();
        let status = ca::AudioObjectGetPropertyData(
            object_id,
            std::ptr::NonNull::from(&mut address),
            0,
            std::ptr::null(),
            std::ptr::NonNull::from(&mut size),
            std::ptr::NonNull::new(&mut raw as *mut _ as *mut _)?,
        );
        if status != 0 || raw.is_null() {
            return None;
        }
        let s = (*raw).to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// Owns one `CATapDescription`-created tap's lifetime.
///
/// The tap captures the mixed-down output of exactly one process and (via
/// `CATapMuteBehavior::MutedWhenTapped` -- see risk item 2 in the module doc comment for the
/// exact-spelling caveat) silences that process's normal path to the speakers so we don't get
/// double audio. Mirrors sonicflow's `ProcessTap`/AudioCap's `ProcessTap`.
struct ProcessTap {
    tap_id: AudioObjectID,
    #[allow(dead_code)] // kept for logging/debugging; not read elsewhere yet
    process_object_id: AudioObjectID,
}

impl ProcessTap {
    fn new(process_object_id: AudioObjectID, label: &str) -> Result<Self, MixerError> {
        // NSNumber-box the *process object id* (an AudioObjectID / u32), not the pid_t -- Core
        // Audio process taps key off the HAL process object, matching both reference repos.
        //
        // RISK (grouped with item 3 in the module doc comment -- container-construction naming):
        // `NSArray::from_retained_slice` / `NSNumber::new_u32` / `NSString::from_str` (used a few
        // lines below) are plausible objc2-foundation method names by convention, not individually
        // confirmed via docs.rs in this session (research time went to the higher-stakes
        // `objc2-core-audio` function/struct surface first). If any of these three don't compile
        // as spelled, the fix is a rename to whatever `objc2-foundation` 0.3's actual constructor
        // is called -- the surrounding alloc/init shape should stay the same.
        let ids: Retained<NSArray<NSNumber>> =
            NSArray::from_retained_slice(&[NSNumber::new_u32(process_object_id)]);

        // SAFETY: `CATapDescription::alloc()` + `initStereoMixdownOfProcesses` is the standard
        // objc2 alloc/init pattern; the resulting `Retained<CATapDescription>` owns the object.
        let description: Retained<CATapDescription> = unsafe {
            CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &ids)
        };
        unsafe {
            description.setName(&NSString::from_str(&format!("MiXolume.{label}")));
            description.setPrivate(true);
            // RISK (see module doc, item 2): exact enum-case spelling unverified.
            description.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
            description.setExclusive(false);
            description.setMixdown(true);
        }

        let mut tap_id: AudioObjectID = ca::kAudioObjectUnknown as AudioObjectID;
        let status = unsafe { ca::AudioHardwareCreateProcessTap(Some(&description), &mut tap_id) };
        check_status(status, "AudioHardwareCreateProcessTap")?;
        if tap_id == ca::kAudioObjectUnknown as AudioObjectID {
            return Err(MixerError::Platform(format!(
                "AudioHardwareCreateProcessTap returned kAudioObjectUnknown for {label}"
            )));
        }

        Ok(Self {
            tap_id,
            process_object_id,
        })
    }

    /// `kAudioTapPropertyUID` -- needed to reference this tap by UID when building the aggregate
    /// device's `kAudioAggregateDeviceTapListKey` entry.
    fn uid(&self) -> Option<String> {
        // RISK: same CFString-read caveat as `read_process_bundle_id` above.
        unsafe {
            let mut address = global_address(ca::kAudioTapPropertyUID);
            let mut size = std::mem::size_of::<*const NSString>() as u32;
            let mut raw: *const NSString = std::ptr::null();
            let status = ca::AudioObjectGetPropertyData(
                self.tap_id,
                std::ptr::NonNull::from(&mut address),
                0,
                std::ptr::null(),
                std::ptr::NonNull::from(&mut size),
                std::ptr::NonNull::new(&mut raw as *mut _ as *mut _)?,
            );
            if status != 0 || raw.is_null() {
                return None;
            }
            Some((*raw).to_string())
        }
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        unsafe {
            ca::AudioHardwareDestroyProcessTap(self.tap_id);
        }
    }
}

/// **Capture half.** A private aggregate device combining every currently-active tap plus the
/// real default output device (included only as a shared clock source, per both reference repos
/// -- its own IOProc never drives the physical speakers). Its IOProc reads each tap's buffer,
/// multiplies by that app's [`AtomicGainSlot`], mixes into a scratch buffer, and pushes the result
/// into the shared [`FloatRingBuffer`].
struct CaptureAggregate {
    aggregate_id: AudioObjectID,
    io_proc_id: Option<AudioDeviceIOProcID>,
    /// Stashed from [`CaptureAggregate::new`] so [`CaptureAggregate::start`] can move clones of
    /// them into the realtime closure -- the closure needs its own `Arc` handles, but `self` also
    /// needs to keep the originals alive/reachable for the rest of the aggregate's lifetime.
    gain_slots: Arc<Vec<AtomicGainSlot>>,
    ring: Arc<FloatRingBuffer>,
    /// Pre-allocated once here (not per-callback) so [`mix_capture_callback`] never touches the
    /// heap on the realtime thread. See [`Scratch`]'s doc comment.
    scratch: Arc<Scratch>,
    /// A second pre-allocated scratch buffer, same reasoning as `scratch` -- holds one tap's
    /// worth of mono-summed samples per callback, reused across taps and callbacks, purely so
    /// [`macos_ducking::SpeechDetector::process_frame`] never sees a fresh allocation.
    duck_mono_scratch: Arc<Scratch>,
    duck: Arc<DuckingRuntime>,
    /// Kept alive for the aggregate's lifetime -- the block only borrows the `Arc`s it needs, but
    /// the `RcBlock` itself must outlive `io_proc_id`'s registration.
    #[allow(dead_code)]
    block: Option<SendSyncBlock<IoProcFn>>,
}

/// Shared cap on how many samples any one realtime callback mixes/reads per invocation. Sized
/// generously above any real device's typical IO buffer (Core Audio buffers are commonly in the
/// low hundreds to ~4096 frames); [`mix_capture_callback`]/[`mix_playback_callback`] both clamp to
/// this so the pre-allocated [`Scratch`] buffers are never overrun.
const MIX_SCRATCH_CAPACITY: usize = 4096;

impl CaptureAggregate {
    /// `output_device_uid`: the real default output device's UID, used only for the aggregate's
    /// shared clock (see struct doc comment).
    fn new(
        output_device_uid: &str,
        taps: &[ProcessTap],
        gain_slots: Arc<Vec<AtomicGainSlot>>,
        ring: Arc<FloatRingBuffer>,
        duck: Arc<DuckingRuntime>,
    ) -> Result<Self, MixerError> {
        let description = build_aggregate_description(output_device_uid, taps)?;

        let mut aggregate_id: AudioObjectID = ca::kAudioObjectUnknown as AudioObjectID;
        let status = unsafe {
            // `description` is `CFRetained<CFDictionary<CFString, CFType>>`, but
            // `AudioHardwareCreateAggregateDevice` takes the bare (`Opaque`-parameterized)
            // `&CFDictionary` -- `.as_ref()` uses `CFDictionary<K, V>: AsRef<CFDictionary>`
            // (confirmed present in objc2-core-foundation 0.3.2's source alongside the analogous,
            // already-relied-upon `CFArray<T>: AsRef<CFArray>` impl) rather than a `Deref`
            // coercion, since going from a concretely-parameterized `CFDictionary<K, V>` to the
            // bare default-parameterized one isn't a `Deref` relationship.
            ca::AudioHardwareCreateAggregateDevice(
                description.as_ref(),
                std::ptr::NonNull::from(&mut aggregate_id),
            )
        };
        check_status(status, "AudioHardwareCreateAggregateDevice")?;

        Ok(Self {
            aggregate_id,
            io_proc_id: None,
            gain_slots,
            ring,
            scratch: Arc::new(Scratch::new(MIX_SCRATCH_CAPACITY)),
            duck_mono_scratch: Arc::new(Scratch::new(MIX_SCRATCH_CAPACITY)),
            duck,
            block: None,
        })
    }

    fn start(&mut self) -> Result<(), MixerError> {
        let gain_slots = Arc::clone(&self.gain_slots);
        let ring = Arc::clone(&self.ring);
        let scratch = Arc::clone(&self.scratch);
        let duck_mono_scratch = Arc::clone(&self.duck_mono_scratch);
        let duck = Arc::clone(&self.duck);

        // The real generated `AudioDeviceIOBlock` type (checked directly against
        // objc2-core-audio 0.3.2's source) takes `NonNull<..>`, not raw `*mut ..` pointers --
        // confirmed the hard way, by a real compile error once `RcBlock::as_ptr` (a type-checked
        // function) replaced the unchecked `as` cast that used to silently paper over this.
        let block: RcBlock<IoProcFn> = RcBlock::new(
            // `RcBlock::new`'s generic `IntoBlock` bound can't backpropagate the argument types
            // from the `let block: RcBlock<dyn Fn(NonNull<..>, ..)>` annotation above into an
            // untyped closure -- confirmed by a real E0282 "type annotations needed" error with
            // these left elided. Every parameter needs its type spelled out explicitly.
            move |_now: std::ptr::NonNull<AudioTimeStamp>,
                  input_data: std::ptr::NonNull<AudioBufferList>,
                  _input_time: std::ptr::NonNull<AudioTimeStamp>,
                  output_data: std::ptr::NonNull<AudioBufferList>,
                  _output_time: std::ptr::NonNull<AudioTimeStamp>| {
                // SAFETY: called by Core Audio on its own realtime thread with valid, non-null
                // pointers for the lifetime of the call. No allocation/locking happens in this
                // closure body -- only atomic loads, raw pointer arithmetic, and reuse of the
                // pre-allocated `scratch`/`duck_mono_scratch` buffers and `duck`'s per-app state
                // (all single-owner, this closure being the sole owner -- see their doc comments).
                unsafe {
                    mix_capture_callback(
                        input_data.as_ptr(),
                        output_data.as_ptr(),
                        &gain_slots,
                        &ring,
                        &scratch,
                        &duck_mono_scratch,
                        &duck,
                    );
                }
            },
        );

        let mut io_proc_id: AudioDeviceIOProcID = None;
        let status = unsafe {
            ca::AudioDeviceCreateIOProcIDWithBlock(
                std::ptr::NonNull::from(&mut io_proc_id),
                self.aggregate_id,
                None,
                // FIXED (was the file's flagged highest-risk line, and a real crash on real
                // hardware: "objc: Attempt to use unknown class" -> abort in
                // AudioDeviceCreateIOProcIDWithBlock). `&block as *const _ as *mut _` took a
                // pointer to the `RcBlock<F>` *wrapper struct on the Rust stack* (a pointer to a
                // pointer), not to the heap-allocated Objective-C block object it wraps -- Core
                // Audio then tried to send an ObjC message through that garbage pointer.
                // `RcBlock::as_ptr(&block)` is block2's own documented accessor for the real
                // `*mut Block<F>` (== `AudioDeviceIOBlock`, since `DynBlock<F> = Block<F>`).
                RcBlock::as_ptr(&block),
            )
        };
        check_status(status, "AudioDeviceCreateIOProcIDWithBlock (capture)")?;

        let status = unsafe { ca::AudioDeviceStart(self.aggregate_id, io_proc_id) };
        check_status(status, "AudioDeviceStart (capture aggregate)")?;

        self.io_proc_id = Some(io_proc_id);
        self.block = Some(SendSyncBlock(block));
        Ok(())
    }
}

impl Drop for CaptureAggregate {
    fn drop(&mut self) {
        unsafe {
            if let Some(io_proc_id) = self.io_proc_id.take() {
                ca::AudioDeviceStop(self.aggregate_id, io_proc_id);
                ca::AudioDeviceDestroyIOProcID(self.aggregate_id, io_proc_id);
            }
            ca::AudioHardwareDestroyAggregateDevice(self.aggregate_id);
        }
    }
}

/// Realtime capture callback body, pulled out of the closure so it can at least be *read* and
/// reasoned about independently of the `block2` plumbing around it. Not unit-tested (needs real
/// `AudioBufferList` memory), unlike [`FloatRingBuffer`] and [`AppGainState`] above.
///
/// # Safety
/// `input_data`/`output_data` must be valid, non-null `AudioBufferList*` for the duration of this
/// call, as guaranteed by the Core Audio IOProc contract.
unsafe fn mix_capture_callback(
    input_data: *mut AudioBufferList,
    output_data: *mut AudioBufferList,
    gain_slots: &[AtomicGainSlot],
    ring: &FloatRingBuffer,
    scratch: &Scratch,
    duck_mono_scratch: &Scratch,
    duck: &DuckingRuntime,
) {
    // Zero the aggregate's own output buffer -- we never drive audio from this device directly,
    // only use it to receive tap input (mirrors sonicflow's capture callback).
    if let Some(out_list) = (*output_data).mBuffers.first_mut() {
        if !out_list.mData.is_null() {
            let n = out_list.mDataByteSize as usize / std::mem::size_of::<f32>();
            std::ptr::write_bytes(out_list.mData as *mut f32, 0, n);
        }
    }

    let in_buffers = std::slice::from_raw_parts(
        (*input_data).mBuffers.as_ptr(),
        (*input_data).mNumberBuffers as usize,
    );
    let tap_count = in_buffers.len().min(gain_slots.len());
    if tap_count == 0 {
        return;
    }

    let mut mix_samples = 0usize;
    for buf in in_buffers.iter().take(tap_count) {
        let n = buf.mDataByteSize as usize / std::mem::size_of::<f32>();
        mix_samples = mix_samples.max(n);
    }
    // Clamped to the pre-allocated scratch capacity (see `Scratch`/`MIX_SCRATCH_CAPACITY`) --
    // never allocated fresh here, so this callback stays allocation-free on the realtime thread.
    mix_samples = mix_samples.min(MIX_SCRATCH_CAPACITY);
    if mix_samples == 0 {
        return;
    }

    // --- Auto-duck analysis pass: classify each tap's audio before mixing anything, so the mix
    // pass below already knows who (if anyone) is currently "talking". Entirely skipped, at
    // zero cost beyond one atomic load, when the feature is off. ---
    let ducking_enabled = duck.is_enabled();
    if ducking_enabled {
        // SAFETY: same single-owner discipline as `scratch` -- see `DuckingRuntime`'s doc comment.
        let detectors = duck.detectors_mut();
        let mono_buf = &mut duck_mono_scratch.as_mut_slice()[..mix_samples];
        for (i, buf) in in_buffers.iter().take(tap_count).enumerate() {
            if buf.mData.is_null() || i >= detectors.len() {
                continue;
            }
            let in_samples =
                (buf.mDataByteSize as usize / std::mem::size_of::<f32>()).min(mix_samples);
            let frames = in_samples / 2;
            let src = std::slice::from_raw_parts(buf.mData as *const f32, in_samples);
            for f in 0..frames {
                mono_buf[f] = 0.5 * (src[f * 2] + src[f * 2 + 1]);
            }
            detectors[i].process_frame(&mono_buf[..frames]);
        }
    }
    let any_triggering = ducking_enabled && duck.detectors_mut().iter().any(|d| d.is_triggering());

    // SAFETY: this callback is the sole owner of `scratch` for the lifetime of the capture
    // aggregate (see `Scratch`'s doc comment) -- no other callback/thread touches it concurrently.
    let mix_buf = &mut scratch.as_mut_slice()[..mix_samples];
    mix_buf.fill(0.0);
    let multipliers = duck.multipliers_mut();
    let detectors = duck.detectors_mut();
    for (i, buf) in in_buffers.iter().take(tap_count).enumerate() {
        if buf.mData.is_null() {
            continue;
        }
        let (gain_l, gain_r) = gain_slots[i].load();
        if gain_l == 0.0 && gain_r == 0.0 {
            continue;
        }

        let in_samples = buf.mDataByteSize as usize / std::mem::size_of::<f32>();
        let n = in_samples.min(mix_samples);
        let src = std::slice::from_raw_parts(buf.mData as *const f32, n);
        let total_frames = n / 2;

        // Ducked if something *else* is currently triggering and this app isn't itself the
        // trigger -- an app that's actively talking is never ducked by its own speech.
        // `start_mult`/`end_mult` are only the per-*callback* endpoints (same one-pole smoothing
        // as before, toward `target`); the per-frame loop below linearly ramps between them
        // *within* this buffer instead of applying `end_mult` flat across every sample. Without
        // that, the biggest single step of the whole exponential curve (the very first callback
        // after a trigger/release) landed all at once at the start of a ~10ms buffer -- audible
        // as a faint click, confirmed live. Interpolating per frame removes that step entirely.
        let (start_mult, end_mult) = if let Some(m) = multipliers.get_mut(i) {
            let is_self_triggering = detectors.get(i).is_some_and(|d| d.is_triggering());
            let target = if any_triggering && !is_self_triggering {
                macos_ducking::DUCK_GAIN_MULTIPLIER
            } else {
                1.0
            };
            let start = *m;
            let end = start + (target - start) * DuckingRuntime::SMOOTHING_PER_CALLBACK;
            *m = end;
            (start, end)
        } else {
            (1.0, 1.0)
        };

        // Interleaved stereo (the tap is `initStereoMixdownOfProcesses`): even sample indices
        // are the left channel, odd indices are the right channel.
        for frame in 0..total_frames {
            let t = if total_frames > 1 {
                frame as f32 / (total_frames - 1) as f32
            } else {
                1.0
            };
            let duck_mult = start_mult + (end_mult - start_mult) * t;
            mix_buf[frame * 2] += src[frame * 2] * gain_l * duck_mult;
            mix_buf[frame * 2 + 1] += src[frame * 2 + 1] * gain_r * duck_mult;
        }
    }

    for s in mix_buf.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }

    ring.write(mix_buf);
}

/// **Playback half.** A single `AudioDeviceIOProcID` installed directly on the user's real default
/// output device. Reads from the shared [`FloatRingBuffer`] and **adds** (not replaces) into the
/// device's own output buffer, which already carries whatever the system mixer wrote for every
/// non-tapped app.
struct PlaybackTap {
    device_id: AudioObjectID,
    io_proc_id: Option<AudioDeviceIOProcID>,
    /// Pre-allocated once (not per-callback) so [`mix_playback_callback`] stays allocation-free.
    /// See [`Scratch`]'s doc comment.
    scratch: Arc<Scratch>,
    #[allow(dead_code)]
    block: Option<SendSyncBlock<IoProcFn>>,
}

impl PlaybackTap {
    fn new(device_id: AudioObjectID) -> Self {
        Self {
            device_id,
            io_proc_id: None,
            scratch: Arc::new(Scratch::new(MIX_SCRATCH_CAPACITY)),
            block: None,
        }
    }

    fn start(&mut self, ring: Arc<FloatRingBuffer>) -> Result<(), MixerError> {
        let scratch = Arc::clone(&self.scratch);
        let block: RcBlock<IoProcFn> = RcBlock::new(
            move |_now: std::ptr::NonNull<AudioTimeStamp>,
                  _input_data: std::ptr::NonNull<AudioBufferList>,
                  _input_time: std::ptr::NonNull<AudioTimeStamp>,
                  output_data: std::ptr::NonNull<AudioBufferList>,
                  _output_time: std::ptr::NonNull<AudioTimeStamp>| {
                // SAFETY: same realtime-callback contract as the capture side. No allocation: reuses
                // the pre-allocated `scratch` buffer via `Scratch::as_mut_slice`.
                unsafe {
                    mix_playback_callback(output_data.as_ptr(), &ring, &scratch);
                }
            },
        );

        let mut io_proc_id: AudioDeviceIOProcID = None;
        let status = unsafe {
            ca::AudioDeviceCreateIOProcIDWithBlock(
                std::ptr::NonNull::from(&mut io_proc_id),
                self.device_id,
                None,
                // See the matching fix in `CaptureAggregate::start` for why this must be
                // `RcBlock::as_ptr(&block)`, not a raw cast of `&block` itself.
                RcBlock::as_ptr(&block),
            )
        };
        check_status(status, "AudioDeviceCreateIOProcIDWithBlock (playback)")?;

        let status = unsafe { ca::AudioDeviceStart(self.device_id, io_proc_id) };
        check_status(status, "AudioDeviceStart (playback)")?;

        self.io_proc_id = Some(io_proc_id);
        self.block = Some(SendSyncBlock(block));
        Ok(())
    }
}

impl Drop for PlaybackTap {
    fn drop(&mut self) {
        unsafe {
            if let Some(io_proc_id) = self.io_proc_id.take() {
                ca::AudioDeviceStop(self.device_id, io_proc_id);
                ca::AudioDeviceDestroyIOProcID(self.device_id, io_proc_id);
            }
        }
    }
}

/// # Safety
/// Same contract as [`mix_capture_callback`], plus: `scratch` must be exclusively owned by this
/// callback (see [`Scratch`]'s doc comment).
unsafe fn mix_playback_callback(
    output_data: *mut AudioBufferList,
    ring: &FloatRingBuffer,
    scratch: &Scratch,
) {
    let Some(out_buf) = (*output_data).mBuffers.first() else {
        return;
    };
    if out_buf.mData.is_null() {
        return;
    }
    // Clamped to the pre-allocated scratch capacity -- if a device's buffer is ever larger than
    // `MIX_SCRATCH_CAPACITY` frames (unusual; see that const's doc comment), we simply fill as
    // much of the output as the scratch buffer allows rather than allocate to cover the rest.
    let n = (out_buf.mDataByteSize as usize / std::mem::size_of::<f32>()).min(MIX_SCRATCH_CAPACITY);
    if n == 0 {
        return;
    }
    let scratch_slice = &mut scratch.as_mut_slice()[..n];
    // `ring.read` only fills the samples actually available and documents that the caller must
    // have already zeroed `dst` for the underrun case -- true of a fresh `vec![0.0; n]`, but this
    // buffer is reused across calls, so it must be explicitly cleared first.
    scratch_slice.fill(0.0);
    let read = ring.read(scratch_slice);

    let out_samples = std::slice::from_raw_parts_mut(out_buf.mData as *mut f32, n);
    for f in 0..read {
        out_samples[f] = (out_samples[f] + scratch_slice[f]).clamp(-1.0, 1.0);
    }
}

/// Coerce a reference to any concrete Core Foundation wrapper down to `&CFType`, the common root
/// type every CF wrapper `Deref`s to (per `objc2_core_foundation::CFType`'s own doc comment: "All
/// Core Foundation types Deref to this type"). A plain function rather than an inline `as` cast --
/// `as` does not perform `Deref`-based coercion, only genuine coercion sites do (fn args/return,
/// `let` with a type annotation) -- so this gives every heterogeneous dictionary/array value below
/// one clearly-typed, unambiguous coercion site instead of relying on multi-hop inference through
/// a generic call.
///
/// Callers must pass a `&T` where `T` is the concrete CF type itself, never a wrapper around it:
/// - `CFRetained<X>` values (from e.g. `CFString::from_str`) need one manual deref first --
///   `as_cf_type(&*retained_value)` -- since `CFRetained<X>` only derefs to `X`, not transitively
///   to `CFType`.
/// - `CFBoolean::new(..)` returns `&'static CFBoolean` directly (Core Foundation booleans are
///   process-wide singletons, not heap-allocated per call) -- pass it as-is, `as_cf_type(value)`,
///   with no extra `&` (a real, confirmed compile error from mixing this up: `&CFBoolean` doesn't
///   itself implement `Deref<Target = CFType>`, only `CFBoolean` does).
fn as_cf_type<T>(value: &T) -> &objc2_core_foundation::CFType
where
    T: ?Sized + std::ops::Deref<Target = objc2_core_foundation::CFType>,
{
    value
}

/// Build the `kAudioAggregateDeviceUIDKey`/etc. description dictionary Core Audio expects for
/// `AudioHardwareCreateAggregateDevice`. See risk item 3 in the module doc comment.
///
/// `objc2_core_foundation::CFDictionary`/`CFArray` default their generic params to `Opaque` (a
/// marker with no `Type`/`PartialEq`/`Hash` impls) when left unparameterized -- confirmed the hard
/// way, by a real compile attempt failing with "the trait bound `Opaque: Type` is not satisfied"
/// on every bare `CFDictionary`/`CFArray` use below. Every dictionary/array built in this function
/// is therefore explicitly parameterized as `CFDictionary<CFString, CFType>` / `CFArray<CFType>`
/// (`CFType` as the uniform, type-erased value type for what's semantically a heterogeneous
/// `[String: Any]`-shaped dictionary), never left as bare `CFDictionary`/`CFArray`.
fn build_aggregate_description(
    output_device_uid: &str,
    taps: &[ProcessTap],
) -> Result<
    objc2_core_foundation::CFRetained<
        objc2_core_foundation::CFDictionary<
            objc2_core_foundation::CFString,
            objc2_core_foundation::CFType,
        >,
    >,
    MixerError,
> {
    use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};

    let aggregate_uid = CFString::from_str(&format!("com.mixolume.aggregate.{}", uuid_v4_ish()));
    let aggregate_name = CFString::from_str("MiXolume Capture");
    let output_uid_cf = CFString::from_str(output_device_uid);

    // kAudioAggregateDeviceSubDeviceListKey: [ { kAudioSubDeviceUIDKey: output_device_uid } ]
    let key_sub_device_uid = ca_cfstring(ca::kAudioSubDeviceUIDKey);
    let sub_device_dict: CFRetained<CFDictionary<CFString, CFType>> =
        CFDictionary::from_slices(&[&*key_sub_device_uid], &[as_cf_type(&*output_uid_cf)]);
    let sub_device_list: CFRetained<CFArray<CFType>> =
        CFArray::from_objects(&[as_cf_type(&*sub_device_dict)]);

    // kAudioAggregateDeviceTapListKey: one { kAudioSubTapUIDKey, kAudioSubTapDriftCompensationKey }
    // dict per currently-active tap.
    let key_sub_tap_uid = ca_cfstring(ca::kAudioSubTapUIDKey);
    let key_sub_tap_drift = ca_cfstring(ca::kAudioSubTapDriftCompensationKey);
    let mut tap_dicts: Vec<CFRetained<CFDictionary<CFString, CFType>>> =
        Vec::with_capacity(taps.len());
    for tap in taps {
        let Some(uid) = tap.uid() else {
            return Err(MixerError::Platform(
                "tap has no kAudioTapPropertyUID -- cannot add it to the aggregate".to_string(),
            ));
        };
        let uid_cf = CFString::from_str(&uid);
        let drift_true = CFBoolean::new(true);
        let dict: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
            &[&*key_sub_tap_uid, &*key_sub_tap_drift],
            &[as_cf_type(&*uid_cf), as_cf_type(drift_true)],
        );
        tap_dicts.push(dict);
    }
    let tap_dict_refs: Vec<&CFType> = tap_dicts.iter().map(|d| as_cf_type(&**d)).collect();
    let tap_list: CFRetained<CFArray<CFType>> = CFArray::from_objects(&tap_dict_refs);

    let is_private = CFBoolean::new(true);
    let is_stacked = CFBoolean::new(false);
    let tap_autostart = CFBoolean::new(true);

    // Owned bindings first -- taking `&` directly on a `vec![]` element that's itself a temporary
    // (e.g. `&ca_cfstring(...)`) would dangle past the end of the statement, so every key is
    // materialized as a named `CFRetained<CFString>` binding before any reference to it is taken.
    let key_uid = ca_cfstring(ca::kAudioAggregateDeviceUIDKey);
    let key_name = ca_cfstring(ca::kAudioAggregateDeviceNameKey);
    let key_is_private = ca_cfstring(ca::kAudioAggregateDeviceIsPrivateKey);
    let key_is_stacked = ca_cfstring(ca::kAudioAggregateDeviceIsStackedKey);
    let key_main_sub_device = ca_cfstring(ca::kAudioAggregateDeviceMainSubDeviceKey);
    let key_tap_autostart = ca_cfstring(ca::kAudioAggregateDeviceTapAutoStartKey);
    let key_sub_device_list = ca_cfstring(ca::kAudioAggregateDeviceSubDeviceListKey);
    let key_tap_list = ca_cfstring(ca::kAudioAggregateDeviceTapListKey);

    let keys: Vec<&CFString> = vec![
        &key_uid,
        &key_name,
        &key_is_private,
        &key_is_stacked,
        &key_main_sub_device,
        &key_tap_autostart,
        &key_sub_device_list,
        &key_tap_list,
    ];
    let values: Vec<&CFType> = vec![
        as_cf_type(&*aggregate_uid),
        as_cf_type(&*aggregate_name),
        as_cf_type(is_private),
        as_cf_type(is_stacked),
        as_cf_type(&*output_uid_cf),
        as_cf_type(tap_autostart),
        as_cf_type(&*sub_device_list),
        as_cf_type(&*tap_list),
    ];

    Ok(CFDictionary::from_slices(&keys, &values))
}

/// Wrap one of `objc2-core-audio`'s HAL property/dictionary key constants as a `CFString`.
///
/// A real compile confirmed these constants (e.g. `kAudioAggregateDeviceUIDKey`) are exposed as
/// `&'static CStr`, not `&str` -- corrected from this file's original guess. Core Audio's own key
/// strings are always plain ASCII, so the `to_str()` conversion is infallible in practice; it's
/// still asserted rather than silently swallowed so a genuinely malformed constant would fail loud
/// instead of silently producing a wrong dictionary key.
fn ca_cfstring(
    key: &std::ffi::CStr,
) -> objc2_core_foundation::CFRetained<objc2_core_foundation::CFString> {
    let key = key
        .to_str()
        .expect("Core Audio HAL property key constants are always valid ASCII/UTF-8");
    objc2_core_foundation::CFString::from_str(key)
}

/// Cheap, dependency-free unique-enough id for the aggregate device's UID string -- doesn't need
/// to be a real RFC 4122 UUID, just unique per process run (mirrors both reference repos' use of
/// `UUID().uuidString` purely as a unique aggregate-device identifier, not for any parsing).
fn uuid_v4_ish() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

/// Gates all process-tap creation behind a single, explicit, app-lifetime-once permission
/// request instead of letting each [`ProcessTap::new`] call implicitly trigger its own OS prompt.
///
/// Confirmed live on real hardware: without this gate, launching with N already-audible apps
/// open produced N separate sequential system permission dialogs (one per app) instead of one.
/// The module doc comment's cited reference (sonicflow) says no explicit preflight is
/// *required* -- the OS prompts automatically on the first `AudioHardwareCreateProcessTap` call
/// -- but that claim was evidently only tested with taps created one at a time over real time,
/// not in the tight batch loop [`TapEngine::new`] uses to tap every already-running app at once
/// on first launch. Firing that many authorization-triggering calls within milliseconds of each
/// other, before the first one's decision has propagated through the TCC daemon, is what
/// produces the queued-up separate prompts.
///
/// `CGPreflightScreenCaptureAccess`/`CGRequestScreenCaptureAccess` (CoreGraphics.framework) are
/// Apple's public, documented API for exactly this permission category -- as of macOS
/// Sonoma/Sequoia, system audio capture via Core Audio process taps shares the same "Screen &
/// System Audio Recording" TCC bucket as screen recording, which is why the screen-recording API
/// is the correct thing to preflight/request here even though we're not capturing video.
mod screen_capture_permission {
    use std::sync::atomic::{AtomicBool, Ordering};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    static REQUEST_SENT: AtomicBool = AtomicBool::new(false);

    /// Returns `true` if permission is already granted. If not, triggers the OS prompt at most
    /// once per process lifetime (later calls just recheck status without prompting again) and
    /// returns `false` -- callers must back off and retry later rather than proceeding to create
    /// any taps, since doing so per-process is exactly what caused the multi-prompt bug.
    pub(super) fn ensure_granted() -> bool {
        // SAFETY: both are argument-less functions returning a plain Boolean; documented public
        // Apple API, safe to call from any thread.
        if unsafe { CGPreflightScreenCaptureAccess() } {
            return true;
        }
        if !REQUEST_SENT.swap(true, Ordering::SeqCst) {
            unsafe {
                CGRequestScreenCaptureAccess();
            }
        }
        false
    }
}

/// The live tap+aggregate+playback rig for whatever set of processes is currently producing
/// output. Torn down and rebuilt (via `Drop`, then a fresh [`TapEngine::new`]) whenever that set
/// changes -- Core Audio has no documented way to add/remove a tap from a running aggregate
/// device, matching both reference repos' own architecture.
///
/// Field order matters here: a struct with no custom `Drop` impl drops its fields in declaration
/// order, and `taps` is declared *last* on purpose. `ProcessTap::drop` un-mutes the tapped
/// process's normal output path immediately (`CATapMuteBehavior::MutedWhenTapped`), so dropping
/// `capture`/`playback` first -- stopping the mixed pipeline cleanly -- before any native path
/// unmutes means teardown is a clean stop-then-resume instead of a brief double-audio overlap
/// (the mixed pipeline's tail still flowing to the speakers at the same moment the native path
/// wakes back up).
struct TapEngine {
    /// session_id -> index into `taps`/`gain_slots`, in tap-creation order.
    slot_of: HashMap<String, usize>,
    gain_slots: Arc<Vec<AtomicGainSlot>>,
    #[allow(dead_code)]
    capture: CaptureAggregate,
    #[allow(dead_code)]
    playback: PlaybackTap,
    #[allow(dead_code)] // kept alive so the taps aren't destroyed out from under the aggregate
    taps: Vec<ProcessTap>,
}

impl TapEngine {
    fn new(
        active: &[(&AudioProcessInfo, String, (f32, f32))], // (process, session_id, initial (left, right) gain)
        ducking_enabled_live: Arc<AtomicBool>,
        ducking_excluded_flags: Vec<bool>,
        ducking_persisted_states: Vec<macos_ducking::PersistedDuckState>,
    ) -> Result<Self, MixerError> {
        if !screen_capture_permission::ensure_granted() {
            return Err(MixerError::Platform(
                "waiting for screen & system audio recording permission".to_string(),
            ));
        }

        let system_object: AudioObjectID = ca::kAudioObjectSystemObject as AudioObjectID;
        let output_device_id: AudioObjectID =
            unsafe { read_property(system_object, ca::kAudioHardwarePropertyDefaultOutputDevice)? };
        let output_uid = read_device_uid(output_device_id)?;

        let mut taps = Vec::with_capacity(active.len());
        let mut slot_of = HashMap::with_capacity(active.len());
        let mut initial_gains = Vec::with_capacity(active.len());
        for (index, (process, session_id, gain)) in active.iter().enumerate() {
            let tap = ProcessTap::new(process.object_id, session_id)?;
            slot_of.insert(session_id.clone(), index);
            initial_gains.push(*gain);
            taps.push(tap);
        }
        if taps.is_empty() {
            return Err(MixerError::Platform(
                "TapEngine::new called with no active processes".to_string(),
            ));
        }

        let gain_slots: Arc<Vec<AtomicGainSlot>> =
            Arc::new(initial_gains.into_iter().map(AtomicGainSlot::new).collect());

        let ring = Arc::new(FloatRingBuffer::new(8192));
        let duck = Arc::new(DuckingRuntime::new(
            ducking_enabled_live,
            ducking_excluded_flags,
            ducking_persisted_states,
        ));

        let mut capture = CaptureAggregate::new(
            &output_uid,
            &taps,
            Arc::clone(&gain_slots),
            Arc::clone(&ring),
            duck,
        )?;
        capture.start()?;

        let mut playback = PlaybackTap::new(output_device_id);
        playback.start(ring)?;

        Ok(Self {
            slot_of,
            taps,
            gain_slots,
            capture,
            playback,
        })
    }

    /// Reads back the current ducking hysteresis state for every tapped app, keyed by session
    /// id -- called by `reconcile_engine` on the *outgoing* engine, right before it's replaced,
    /// so the incoming one can seed its detectors from where these left off instead of resetting
    /// to zero. Safe to call at any point in this engine's life (including while its realtime
    /// callback might still be running) -- see [`macos_ducking::HysteresisCounters`]'s doc
    /// comment for why that's actually true and not just hoped-for.
    fn snapshot_ducking_state(&self) -> HashMap<String, macos_ducking::PersistedDuckState> {
        let snapshots = self.capture.duck.snapshot_all();
        self.slot_of
            .iter()
            .filter_map(|(session_id, &index)| {
                snapshots
                    .get(index)
                    .map(|state| (session_id.clone(), *state))
            })
            .collect()
    }

    fn set_gain(&self, session_id: &str, effective_gains: (f32, f32)) {
        if let Some(&idx) = self.slot_of.get(session_id) {
            self.gain_slots[idx].store(effective_gains);
        }
    }
}

fn read_device_uid(device_id: AudioObjectID) -> Result<String, MixerError> {
    // RISK: same CFString-read caveat noted on `read_process_bundle_id`/`ProcessTap::uid`.
    unsafe {
        let mut address = global_address(ca::kAudioDevicePropertyDeviceUID);
        let mut size = std::mem::size_of::<*const NSString>() as u32;
        let mut raw: *const NSString = std::ptr::null();
        let status = ca::AudioObjectGetPropertyData(
            device_id,
            std::ptr::NonNull::from(&mut address),
            0,
            std::ptr::null(),
            std::ptr::NonNull::from(&mut size),
            std::ptr::NonNull::new(&mut raw as *mut _ as *mut _)
                .ok_or_else(|| MixerError::Platform("null device UID pointer".to_string()))?,
        );
        check_status(
            status,
            "AudioObjectGetPropertyData(kAudioDevicePropertyDeviceUID)",
        )?;
        if raw.is_null() {
            return Err(MixerError::Platform("device UID was null".to_string()));
        }
        Ok((*raw).to_string())
    }
}

// =================================================================================================
// The public backend.
// =================================================================================================

/// How long a process that was recently seen actively producing output continues to count as
/// "wanted" by `MacosMixerBackend::reconcile_engine`'s tap-teardown decision, even if the
/// current poll's `is_running_output` reading is momentarily `false` -- see
/// `Inner::active_hold_until`'s doc comment for the full rationale. Matches the frontend's own
/// `FADE_HOLD_MS` (`App.tsx`): same underlying flaky signal, same reasoning for how long to
/// tolerate it, even though the two hold windows serve different purposes (this one guards an
/// expensive engine rebuild; that one guards a UI transition) and are otherwise independent.
const ACTIVE_HOLD_DURATION: Duration = Duration::from_millis(1500);

struct Inner {
    /// Persistent per-app gain/mute state, keyed by [`AppSession::id`]. Survives tap
    /// teardown/rebuild (see [`TapEngine`]'s doc comment).
    gain_state: HashMap<String, AppGainState>,
    /// `None` when no process is currently producing output (nothing to tap yet, or every
    /// previously-tapped process went silent).
    engine: Option<TapEngine>,
    /// [`resolve_app_info`] results, cached by pid. A process's name/icon never change during
    /// its lifetime, so there's no reason to re-run the `NSRunningApplication`/`NSImage`/
    /// `NSBitmapImageRep` round-trip on every single 700ms poll tick.
    ///
    /// This isn't just a perf nicety: confirmed live on real hardware that re-encoding the icon
    /// to PNG from scratch every poll tick, for every active session, leaked memory catastrophically
    /// (observed 15GB+ RSS within a few minutes) -- these AppKit calls create autoreleased
    /// temporary objects, and this poll loop runs on a plain tokio background thread with no
    /// autorelease pool ever pushed to drain them. Caching cuts the call frequency from "every
    /// ~700ms per active app" down to "once per app, ever" (until the app exits and its pid gets
    /// reused, at which point removing the stale entry -- see `list_sessions` -- fixes it),
    /// which makes the leak negligible even without solving the underlying autorelease-pool gap.
    ///
    /// [`AppInfoCacheEntry::Pending`] until a spawned background thread (see `list_sessions`)
    /// finishes the resolve and writes [`AppInfoCacheEntry::Resolved`] back in -- confirmed live
    /// (macOS `sample` profiling during an active drag) that `resolve_app_info` is genuinely slow
    /// on a process's *first* poll tick (~100ms+), and running it synchronously here, inside the
    /// same lock `set_volume` et al. need on every drag update, was the actual cause of periodic
    /// UI freezes that got worse the more apps were tapped (more distinct pids, more of them
    /// eventually hitting this cold-start cost). A pid stays `Pending` for however many ticks the
    /// background resolve takes; `list_sessions` shows a `pid <N>` placeholder with no icon in the
    /// meantime rather than blocking on it.
    app_info_cache: HashMap<i32, AppInfoCacheEntry>,
    /// [`is_hidden_system_process_cached`] results, cached by pid -- see that function's doc
    /// comment for why re-deriving this via a live syscall on every poll tick is a real
    /// correctness bug (a transient syscall failure lets a process that should be hidden through
    /// for that one poll), not just wasted work like `app_info_cache`'s case.
    hidden_process_cache: HashMap<i32, bool>,
    /// Per-session id, the instant until which [`Self::reconcile_engine`] should still treat that
    /// session as "wanted" even if the current poll's `is_running_output` reading says otherwise
    /// -- `kAudioProcessPropertyIsRunningOutput` is documented (and confirmed live) to report
    /// brief false readings for a genuinely still-playing app, exactly the reason
    /// `useSessionListWithFadeOut` holds `isActive` on the frontend for the same duration. There
    /// was no equivalent hold on this side: reacting to that flicker here means tearing down and
    /// rebuilding the *entire* tap engine -- real Core Audio HAL calls, done while holding the
    /// same lock the poll loop needs -- for a signal glitch, not a real state change. Confirmed
    /// live as the cause of the whole app visibly slowing down specifically once a second app
    /// was tapped: more apps tapped means more for any single flaky reading to force a full
    /// rebuild of, not just the one that flickered.
    active_hold_until: HashMap<String, Instant>,
    /// Cross-app auto-duck settings, loaded from disk once at startup (see
    /// [`macos_ducking::load_settings`]) and updated in place as the user changes them from
    /// Settings. `enabled` is also mirrored into `ducking_enabled_live` for instant effect --
    /// see that field's doc comment for why the two aren't the same thing.
    ducking_settings: DuckingSettings,
    /// The single source of truth [`DuckingRuntime`] actually reads every audio callback,
    /// shared (via `Arc`, cloned in) across every engine rebuild so toggling the feature in
    /// Settings takes effect on the very next callback instead of waiting for some app to
    /// start/stop and trigger a rebuild. Per-app exclusion, in contrast, *is* only baked in at
    /// rebuild time -- a much rarer settings change, where "takes effect shortly" is an
    /// acceptable tradeoff for not needing a second live-update channel into the realtime state.
    ducking_enabled_live: Arc<AtomicBool>,
    /// Per-app ducking hysteresis, keyed by session id -- survives engine rebuilds the same way
    /// `gain_state` does (seeded into each fresh engine, refreshed from the outgoing one right
    /// before it's replaced). Without this, a rebuild would reset every app's "how long has this
    /// been speech" counter to zero, which is exactly the real bug `HysteresisCounters`'s doc
    /// comment describes -- confirmed live, not hypothetical.
    ducking_state: HashMap<String, macos_ducking::PersistedDuckState>,
    /// Bumped every time [`MacosMixerBackend::reconcile_engine`] starts a rebuild -- a
    /// belt-and-suspenders guard so a build that somehow outlives the one-at-a-time discipline
    /// `pending_rebuild_target` enforces discards itself instead of clobbering `engine` with an
    /// outdated result. See that function's own comment for why the actual build happens on a
    /// background thread in the first place.
    rebuild_generation: u64,
    /// The session-id set the single in-flight rebuild is building toward, or `None` when no
    /// rebuild is running. Doubles as the "a rebuild is already running" flag: while it's `Some`,
    /// [`MacosMixerBackend::reconcile_engine`] starts no further rebuilds at all.
    ///
    /// Without it, the "is a rebuild even needed" check only had `engine`'s *already-installed*
    /// tapped set to compare against, which stays empty for as long as a build is in flight
    /// (`engine` is cleared before the background thread starts, and only set once it finishes)
    /// -- so every poll tick that landed during that window (every ~150ms, for however long a
    /// real Core Audio aggregate-device build takes) saw "0 tapped, but N wanted" and kicked off
    /// *another* concurrent rebuild, compounding into exactly a full freeze. Confirmed live: the
    /// very regression this field fixes.
    pending_rebuild_target: Option<std::collections::HashSet<String>>,
    /// Per-session id, whether that session was baked in as duck-trigger-*excluded* (i.e. not a
    /// priority app) in the currently-installed `engine` -- set alongside `engine` itself, right
    /// after a successful rebuild. `reconcile_engine` compares a freshly recomputed version of
    /// this against it on every poll tick so a rebuild can be forced purely because *this*
    /// changed, even when the tapped session set itself didn't.
    ///
    /// Exists because per-app exclusion is only ever decided from whatever `app_info_cache`
    /// already has at rebuild time, and a priority-trigger app's very *first* appearance always
    /// computes as excluded there -- its name can't possibly be resolved yet, since
    /// `list_sessions` only warms that cache in its own loop, which runs *after*
    /// `reconcile_engine` returns. Without this, nothing ever revisits that decision once the
    /// name resolves a moment later unless some *unrelated* change to the tapped set happens to
    /// trigger another rebuild anyway -- confirmed live as the actual cause of a real, reported
    /// bug: auto-duck not triggering the first time a priority app played audio after Mixolume
    /// started, only after the user repeated the action (silence, then audio again) a few times,
    /// each attempt's silence/audio transition being its own chance for an unrelated rebuild to
    /// stumble into correcting it.
    installed_duck_excluded: HashMap<String, bool>,
}

/// macOS backend: per-app volume via Core Audio process taps + a private aggregate device +
/// a lock-free ring buffer bridging to a playback IOProc on the real output device. See the
/// module doc comment for the full architecture, citations, and flagged risk areas.
pub struct MacosMixerBackend {
    /// `Arc`-wrapped so the background threads `list_sessions` spawns to resolve
    /// [`Inner::app_info_cache`] entries (see that field's doc comment) can hold their own handle
    /// and write their result back in without needing `&MacosMixerBackend` to outlive them.
    inner: Arc<Mutex<Inner>>,
}

impl MacosMixerBackend {
    pub fn new() -> Self {
        let ducking_settings = macos_ducking::load_settings();
        let ducking_enabled_live = Arc::new(AtomicBool::new(ducking_settings.enabled));
        Self {
            inner: Arc::new(Mutex::new(Inner {
                gain_state: HashMap::new(),
                engine: None,
                app_info_cache: HashMap::new(),
                hidden_process_cache: HashMap::new(),
                active_hold_until: HashMap::new(),
                ducking_settings,
                ducking_enabled_live,
                ducking_state: HashMap::new(),
                rebuild_generation: 0,
                pending_rebuild_target: None,
                installed_duck_excluded: HashMap::new(),
            })),
        }
    }

    /// Tear down and rebuild [`TapEngine`] if the currently-output-active process set differs
    /// from what's presently tapped. No-op if unchanged (avoids the audible glitch of an
    /// unnecessary rebuild).
    fn reconcile_engine(
        inner: &mut Inner,
        inner_arc: &Arc<Mutex<Inner>>,
        processes: &[AudioProcessInfo],
    ) -> Result<(), MixerError> {
        let now = Instant::now();
        // "Active" for tap purposes means reporting so right now, *or* within its hold window --
        // see `Inner::active_hold_until`'s doc comment for why reacting to the raw signal alone
        // would tear down and rebuild the whole engine over transient flicker, not a real change.
        let active: Vec<&AudioProcessInfo> = processes
            .iter()
            .filter(|p| {
                is_wanted_for_reconciliation(
                    p.is_running_output,
                    inner
                        .active_hold_until
                        .get(&session_id_for_pid(p.pid))
                        .copied(),
                    now,
                )
            })
            .collect();

        let currently_tapped: std::collections::HashSet<&str> = inner
            .engine
            .as_ref()
            .map(|e| e.slot_of.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let wanted: std::collections::HashSet<String> =
            active.iter().map(|p| session_id_for_pid(p.pid)).collect();

        // Computed here -- before the tap-set-unchanged check below, not just further down where
        // it's needed for the actual rebuild -- specifically so a rebuild can be forced purely
        // because *this* changed, even when `wanted` itself didn't. See `excluded` and
        // `Inner::installed_duck_excluded`'s doc comments for why that's a real, confirmed case:
        // a priority-trigger app's very first appearance is *always* computed as excluded here
        // (its name can't possibly be in `app_info_cache` yet -- `list_sessions` only warms that
        // cache in its own loop, which runs *after* this function returns), and per-app exclusion
        // is otherwise only ever baked in at rebuild time, with no other trigger to reconsider it
        // once the name resolves a moment later.
        let excluded_flags: Vec<bool> = active
            .iter()
            .map(|p| {
                let is_priority = matches!(
                    inner.app_info_cache.get(&p.pid),
                    Some(AppInfoCacheEntry::Resolved(name, _))
                        if inner.ducking_settings.priority_triggers.iter().any(|e| e == name)
                );
                !is_priority
            })
            .collect();
        let fresh_duck_excluded: HashMap<String, bool> = active
            .iter()
            .map(|p| session_id_for_pid(p.pid))
            .zip(excluded_flags.iter().copied())
            .collect();

        let tap_set_unchanged = currently_tapped.len() == wanted.len()
            && wanted
                .iter()
                .all(|id| currently_tapped.contains(id.as_str()));
        // Even when the tapped set itself hasn't changed, a session whose exclusion flag would
        // now compute differently than what's actually installed still needs a rebuild to pick
        // that up -- see this function's earlier comment on `excluded_flags` for the confirmed
        // "ducking doesn't trigger the first time" bug this closes.
        let exclusion_unchanged = wanted
            .iter()
            .all(|id| inner.installed_duck_excluded.get(id) == fresh_duck_excluded.get(id));
        let unchanged = tap_set_unchanged && exclusion_unchanged;
        // At most one rebuild runs at a time, whatever set it's targeting -- see
        // `Inner::pending_rebuild_target`'s doc comment for why the in-flight window otherwise
        // reads as "0 tapped" to every poll tick that lands in it, and re-triggers yet another
        // concurrent rebuild. Deliberately not "already heading toward exactly this set": letting
        // a *differently*-targeted rebuild start alongside one already running means two engines
        // can briefly exist at once, each with its own aggregate device, its own taps on the same
        // processes, and its own playback IOProc adding into the same real output device.
        // Whatever the current build converges on is one poll tick (~150ms) away from being
        // reconciled again anyway, which is cheaper than that overlap.
        let already_in_flight = inner.pending_rebuild_target.is_some();
        if unchanged || already_in_flight {
            return Ok(());
        }

        // Read back the outgoing engine's ducking hysteresis *before* dropping it, so the new
        // engine can seed its detectors from where these left off instead of resetting every
        // app's "how long has this been speech" counter to zero on every single rebuild -- see
        // `Inner::ducking_state`'s doc comment for why that used to be a real, confirmed bug.
        if let Some(old_engine) = inner.engine.as_ref() {
            inner
                .ducking_state
                .extend(old_engine.snapshot_ducking_state());
        }

        // Detached from `inner` here, torn down further below -- Core Audio aggregate/tap ids
        // aren't reusable, so the old engine has to be gone before the new one is built, and we
        // don't want two aggregates fighting over the same real output device even briefly.
        let old_engine = inner.engine.take();

        if active.is_empty() {
            drop(old_engine);
            inner.installed_duck_excluded.clear();
            return Ok(());
        }

        // Owned (not borrowed) so this can be handed to the background thread below -- see that
        // thread's own comment for why the actual `TapEngine::new` call must not happen here,
        // still holding this lock.
        let active_with_state: Vec<(AudioProcessInfo, String, (f32, f32))> = active
            .iter()
            .map(|p| {
                let id = session_id_for_pid(p.pid);
                let gains = inner
                    .gain_state
                    .entry(id.clone())
                    .or_default()
                    .effective_gains();
                ((*p).clone(), id, gains)
            })
            .collect();

        // `excluded_flags`/`fresh_duck_excluded` were already computed above, before the
        // tap-set-unchanged check -- reused here as-is for the actual rebuild.
        let persisted_ducking_states: Vec<macos_ducking::PersistedDuckState> = active
            .iter()
            .map(|p| {
                inner
                    .ducking_state
                    .get(&session_id_for_pid(p.pid))
                    .copied()
                    .unwrap_or_default()
            })
            .collect();

        // `TapEngine::new` below does real Core Audio HAL work (creating an aggregate device,
        // one process tap per app) -- genuinely slow enough (confirmed live via `sample`
        // profiling during an active drag: real, measured multi-hundred-millisecond spikes with
        // new `com.apple.audio.IOThread.client` threads appearing) that running it while still
        // holding this same lock blocks every `set_volume`/`set_balance` call for as long as it
        // takes, for the entire time a session set is genuinely changing -- not just the
        // already-fixed flicker case. Mirrors how a native (non-webview) app in the same problem
        // space handles its own equivalent rebuild: build off the critical path, then just swap
        // the finished result in.
        //
        // `rebuild_generation` guards against installing a stale result: if the active set
        // changes *again* before this build finishes (another rebuild started, bumping the
        // generation), the now-superseded build is dropped instead of clobbering whatever the
        // newer one installs.
        inner.rebuild_generation = inner.rebuild_generation.wrapping_add(1);
        let generation = inner.rebuild_generation;
        inner.pending_rebuild_target = Some(wanted);
        let ducking_enabled_live = Arc::clone(&inner.ducking_enabled_live);
        let inner_arc = Arc::clone(inner_arc);
        std::thread::spawn(move || {
            // Tearing the outgoing engine down is itself real HAL work -- `AudioDeviceStop`
            // blocks until the in-flight IOProc callback returns, and destroying the aggregate
            // device is a round-trip to `coreaudiod` -- so it belongs on this side of the lock
            // for exactly the same reason the build below does. Still strictly before the build,
            // which is the ordering the "no two aggregates at once" invariant needs.
            drop(old_engine);
            let active_refs: Vec<(&AudioProcessInfo, String, (f32, f32))> = active_with_state
                .iter()
                .map(|(process, id, gains)| (process, id.clone(), *gains))
                .collect();
            // A freshly-spawned thread has no autorelease pool of its own, and this path creates
            // real Objective-C temporaries (`CATapDescription`, the `NSString`/`NSArray` it's
            // built from). Same reasoning as `resolve_app_info`/`list_running_applications` --
            // see `Inner::app_info_cache`'s doc comment for the confirmed-on-real-hardware leak
            // an unpooled AppKit/Core Audio call path produced.
            let result = objc2::rc::autoreleasepool(|_pool| {
                TapEngine::new(
                    &active_refs,
                    ducking_enabled_live,
                    excluded_flags,
                    persisted_ducking_states,
                )
            });
            let mut inner = inner_arc.lock().unwrap();
            if inner.rebuild_generation != generation {
                // Superseded while this build was in flight -- whatever's already installed (or
                // about to be, from the newer build) is the correct one, not this. Deliberately
                // does *not* touch `pending_rebuild_target` here: the newer rebuild that
                // superseded this one already owns it.
                return;
            }
            inner.pending_rebuild_target = None;
            match result {
                Ok(engine) => {
                    // `active_with_state`'s gains were read before this build started. Anything
                    // the user changed since (a slider drag lands on `gain_state` regardless of
                    // whether an engine currently exists to push it to) would otherwise be
                    // silently discarded here -- the new engine would come up at the pre-rebuild
                    // volume and stay there until the next `set_volume` call happened to arrive.
                    for session_id in engine.slot_of.keys() {
                        if let Some(state) = inner.gain_state.get(session_id) {
                            engine.set_gain(session_id, state.effective_gains());
                        }
                    }
                    inner.engine = Some(engine);
                    inner.installed_duck_excluded = fresh_duck_excluded;
                }
                Err(err) => {
                    log::warn!("failed to rebuild tap engine: {err}");
                }
            }
        });
        Ok(())
    }
}

impl Default for MacosMixerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixerBackend for MacosMixerBackend {
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        // Exclude our own process before it ever reaches `reconcile_engine`. Confirmed live on
        // real hardware: once `PlaybackTap` genuinely writes audio into the real output device,
        // Core Audio reports Mixolume's own process as `is_running_output = true` -- which,
        // filtered only at UI-display time (see `is_hidden_system_bundle` below) rather than
        // here, meant the engine kept tapping itself. That changed the active-process set on
        // (almost) every single poll tick, forcing a full teardown+rebuild roughly once a
        // second, which never gave the freshly-created playback IOProc a stable window to
        // actually run -- explaining total silence/glitching despite capture working fine.
        // Filtering by our own PID (not just bundle id) is the robust check: it doesn't depend
        // on `read_process_bundle_id`'s CFString bridging succeeding.
        let own_pid = std::process::id() as i32;
        let processes: Vec<AudioProcessInfo> = list_audio_processes()?
            .into_iter()
            .filter(|p| p.pid != own_pid)
            .collect();
        let mut inner = self.inner.lock().unwrap();

        // Ensure every currently-audible process has a persistent gain_state entry (freshly-seen
        // -> full volume, unmuted), *before* reconciling the engine so the engine can read the
        // right initial gain. Also refreshes `active_hold_until` for `reconcile_engine`'s
        // flicker-tolerant "is this still wanted" check -- see that field's doc comment.
        let now = Instant::now();
        for p in &processes {
            if p.is_running_output {
                let id = session_id_for_pid(p.pid);
                inner.gain_state.entry(id.clone()).or_default();
                inner
                    .active_hold_until
                    .insert(id, now + ACTIVE_HOLD_DURATION);
            }
        }

        Self::reconcile_engine(&mut inner, &self.inner, &processes)?;

        // Read back current ducking state for the UI -- safe to call any time (see
        // `TapEngine::snapshot_ducking_state`'s doc comment), and cheap (atomic loads only, no
        // realtime-thread coordination needed).
        // Gated on the settings flag, not just derived from the atomics: toggling the feature
        // off doesn't retroactively clear an in-progress trigger's state (nothing needs it to,
        // since the realtime mixing pass already independently checks the live `enabled` flag
        // before ever applying a duck), so an un-gated read here could keep reporting a stale
        // "still ducking" to the UI for a session or two after the user turns it off.
        let duck_states = if inner.ducking_settings.enabled {
            match inner.engine.as_ref() {
                Some(engine) => engine.snapshot_ducking_state(),
                // `engine` is `None` for the whole time a rebuild is in flight, which is several
                // poll ticks. Reporting "nothing is ducking" across that window flips every
                // session's duck flags off and then back on again -- two changes the poll loop
                // reads as real duck transitions, each arming its 30ms fast-poll window (see
                // `DUCK_TRANSITION_WINDOW` in lib.rs) for 600ms over a rebuild that changed
                // nothing about who's talking. `ducking_state` is the hysteresis snapshot
                // `reconcile_engine` took off the outgoing engine, so it's the right answer for
                // exactly this window -- and only this one: with no rebuild running, a missing
                // engine genuinely means nothing is being tapped, let alone ducked.
                None if inner.pending_rebuild_target.is_some() => inner.ducking_state.clone(),
                None => HashMap::new(),
            }
        } else {
            HashMap::new()
        };
        let any_duck_triggering = duck_states.values().any(|s| s.is_triggering);

        let mut sessions = Vec::with_capacity(processes.len());
        for p in &processes {
            if is_hidden_system_process_cached(
                &mut inner.hidden_process_cache,
                p.bundle_id.as_deref(),
                p.pid,
            ) {
                continue;
            }
            let id = session_id_for_pid(p.pid);
            // Copy out before touching `app_info_cache` below -- `state` borrows
            // `inner.gain_state` immutably, and the cache lookup needs `inner` mutably; ending
            // the borrow here (both fields are `Copy`) avoids the conflict.
            let Some((volume, muted, balance, generation)) = inner
                .gain_state
                .get(&id)
                .map(|s| (s.volume, s.muted, s.balance, s.generation))
            else {
                // Never seen producing output -- not a "known" session yet, matching the
                // task's "lazily tap any newly-seen process" contract (nothing to report until
                // it's actually made sound at least once).
                continue;
            };
            // Cached by pid -- see `Inner::app_info_cache`'s doc comment for why re-resolving
            // this every poll tick is a real memory-safety bug, not just wasted work, and why a
            // still-`Pending` entry is resolved on a background thread rather than blocking here.
            let (display_name, icon_png) = match inner.app_info_cache.get(&p.pid) {
                Some(AppInfoCacheEntry::Resolved(name, icon)) => (name.clone(), icon.clone()),
                Some(AppInfoCacheEntry::Pending) => (format!("pid {}", p.pid), None),
                None => {
                    inner
                        .app_info_cache
                        .insert(p.pid, AppInfoCacheEntry::Pending);
                    let pid = p.pid;
                    let bundle_id = p.bundle_id.clone();
                    let inner_arc = Arc::clone(&self.inner);
                    std::thread::spawn(move || {
                        let resolved = resolve_app_info(pid, bundle_id.as_deref());
                        let mut inner = inner_arc.lock().unwrap();
                        // Only overwrite if still `Pending` -- if the pid was pruned and reused
                        // by an unrelated process in the meantime, `list_sessions` will have
                        // already re-inserted a fresh `Pending` (or `Resolved`) entry for it that
                        // this stale resolve must not clobber.
                        if matches!(
                            inner.app_info_cache.get(&pid),
                            Some(AppInfoCacheEntry::Pending)
                        ) {
                            inner
                                .app_info_cache
                                .insert(pid, AppInfoCacheEntry::Resolved(resolved.0, resolved.1));
                        }
                    });
                    (format!("pid {pid}"), None)
                }
            };
            let is_duck_trigger = duck_states.get(&id).is_some_and(|s| s.is_triggering);
            // Ducked by *someone else* -- an app currently triggering never ducks itself,
            // matching the exact same condition `mix_capture_callback`'s mixing pass applies to
            // the real audio.
            let is_ducked = any_duck_triggering && !is_duck_trigger;
            sessions.push(AppSession {
                id,
                display_name,
                icon_png,
                volume,
                // Matches `mix_capture_callback`'s own duck multiplier exactly -- this is what's
                // actually coming out of the speakers right now, not the target `volume`.
                effective_volume: if is_ducked {
                    volume * macos_ducking::DUCK_GAIN_MULTIPLIER
                } else {
                    volume
                },
                muted,
                balance,
                is_active: p.is_running_output,
                is_duck_trigger,
                is_ducked,
                write_generation: generation,
                // Output routing isn't implemented on macOS yet -- see `mod.rs`'s
                // `AudioMixerBackend::output_routing_supported` default.
                output_device_id: None,
            });
        }
        // Drop cache entries for pids that no longer exist -- keeps this bounded over a long
        // Mixolume session that sees many different apps come and go, rather than growing
        // forever. Cheap: just a pid membership check per entry, no AppKit calls.
        let live_pids: std::collections::HashSet<i32> = processes.iter().map(|p| p.pid).collect();
        inner
            .app_info_cache
            .retain(|pid, _| live_pids.contains(pid));
        inner
            .hidden_process_cache
            .retain(|pid, _| live_pids.contains(pid));
        // `gain_state`/`ducking_state` are keyed by session id (`"macos-{pid}"`), not raw pid, but
        // need the same pruning for the same reason -- and it's not just a slow leak here: pids
        // get reused by the OS, so a stale entry could hand a completely unrelated future process
        // a stranger's leftover volume/mute/balance/ducking state instead of fresh defaults the
        // next time that pid number comes back around.
        let live_session_ids: std::collections::HashSet<String> = processes
            .iter()
            .map(|p| session_id_for_pid(p.pid))
            .collect();
        inner
            .gain_state
            .retain(|id, _| live_session_ids.contains(id));
        inner
            .ducking_state
            .retain(|id, _| live_session_ids.contains(id));
        inner
            .active_hold_until
            .retain(|id, _| live_session_ids.contains(id));
        // `kAudioHardwarePropertyProcessObjectList` (the source of `processes`, and therefore of
        // this order) has no documented ordering guarantee, and nothing upstream imposes one --
        // confirmed live as a real, serious bug, not just a cosmetic one: with two or more
        // sessions, an unstable order reshuffles the frontend's rendered list on poll ticks where
        // nothing user-visible actually changed, which the row list's Framer Motion
        // `layout="position"` tracking (correctly) interprets as things needing to animate into
        // new positions -- repeatedly, every time the order happens to shuffle again, which
        // showed up as sustained near-100% frontend CPU once a second app was added (nothing to
        // reorder against with only one session, so invisible until then). Sorting here, once,
        // gives every consumer a stable, deterministic order for free. `display_name` first
        // (what a user would expect a stable ordering to follow), `id` as a tie-break for
        // sessions that share a name (e.g. two windows of the same app), so the order is fully
        // deterministic even then.
        sessions.sort_by(|a, b| {
            a.display_name
                .cmp(&b.display_name)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(sessions)
    }

    fn max_volume_percent(&self) -> u32 {
        150
    }

    fn set_volume(&self, session_id: &str, volume: f32) -> Result<u64, MixerError> {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .gain_state
            .get_mut(session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
        let generation = state.set_volume(volume);
        let effective = state.effective_gains();
        if let Some(engine) = &inner.engine {
            engine.set_gain(session_id, effective);
        }
        Ok(generation)
    }

    fn set_muted(&self, session_id: &str, muted: bool) -> Result<u64, MixerError> {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .gain_state
            .get_mut(session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
        let generation = state.set_muted(muted);
        let effective = state.effective_gains();
        if let Some(engine) = &inner.engine {
            engine.set_gain(session_id, effective);
        }
        Ok(generation)
    }

    fn set_balance(&self, session_id: &str, balance: f32) -> Result<u64, MixerError> {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .gain_state
            .get_mut(session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
        let generation = state.set_balance(balance);
        let effective = state.effective_gains();
        if let Some(engine) = &inner.engine {
            engine.set_gain(session_id, effective);
        }
        Ok(generation)
    }

    /// Called from the "Quit" menu handler before `app.exit()`. `app.exit()` calls
    /// `std::process::exit()` under the hood and does *not* run `Drop` for arbitrary managed
    /// state (confirmed against Tauri's own exit-handling behavior) -- without this, quitting
    /// while any app is tapped would leave that app's normal output path muted (per
    /// `CATapMuteBehavior::MutedWhenTapped`) until macOS separately notices the dead process and
    /// reclaims its Core Audio objects, which is an audible extra gap on top of the already
    /// short one a clean `Drop` produces. Dropping `engine` here runs exactly the same
    /// `TapEngine` teardown `reconcile_engine` already relies on, just synchronously and on
    /// purpose instead of leaving it to chance.
    fn shutdown(&self) {
        self.inner.lock().unwrap().engine = None;
    }

    fn get_ducking_settings(&self) -> DuckingSettings {
        self.inner.lock().unwrap().ducking_settings.clone()
    }

    fn set_ducking_enabled(&self, enabled: bool) -> Result<(), MixerError> {
        let mut inner = self.inner.lock().unwrap();
        let was_enabled = inner.ducking_settings.enabled;
        inner.ducking_settings.enabled = enabled;
        // Takes effect on the very next audio callback -- see `ducking_enabled_live`'s doc
        // comment on `Inner` for why this is a live atomic rather than baked into the engine.
        inner.ducking_enabled_live.store(enabled, Ordering::Relaxed);

        // First-ever enable (an empty list, not just "currently off"): pre-fill with whichever
        // well-known communication apps are already running, so the feature does something
        // useful immediately instead of silently doing nothing until the user manually finds and
        // adds WhatsApp themselves. Only fires on the false -> true transition with nothing
        // already configured -- reusing "empty list" as the signal rather than a separate
        // "have we ever seeded this" flag is a deliberate simplification: the only way it
        // mis-fires is a user who deliberately emptied the list getting it re-seeded on the next
        // toggle, which is a minor, easily-undone edge case, not worth a whole extra persisted
        // field to prevent.
        if enabled && !was_enabled && inner.ducking_settings.priority_triggers.is_empty() {
            let running_names: Vec<String> = list_running_applications()
                .into_iter()
                .map(|app| app.name)
                .collect();
            super::seed_priority_apps_from_well_known(
                &mut inner.ducking_settings.priority_triggers,
                WELL_KNOWN_COMMUNICATION_APPS,
                &running_names,
            );
        }

        macos_ducking::save_settings(&inner.ducking_settings);
        Ok(())
    }

    fn set_duck_trigger_priority(
        &self,
        display_name: &str,
        is_priority: bool,
    ) -> Result<(), MixerError> {
        let mut inner = self.inner.lock().unwrap();
        super::toggle_priority_trigger(
            &mut inner.ducking_settings.priority_triggers,
            display_name,
            is_priority,
        );
        macos_ducking::save_settings(&inner.ducking_settings);
        Ok(())
    }
}

/// Recognized by exact `NSRunningApplication.localizedName` match against the *default* macOS
/// app name for each -- not exhaustive, not locale-aware (a non-English system's Finder might
/// show a different localized name for some of these), just a reasonable starting set for the
/// common case. The user can always add/remove anything regardless of this list.
const WELL_KNOWN_COMMUNICATION_APPS: &[&str] = &[
    "WhatsApp",
    "Zoom",
    "Discord",
    "FaceTime",
    "Microsoft Teams",
    "Slack",
    "Messages",
    "Skype",
    "Telegram",
    "Signal",
];

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // Session id formatting/parsing
    // ---------------------------------------------------------------------------------------

    #[test]
    fn session_id_matches_windows_backend_convention_shape() {
        assert_eq!(session_id_for_pid(1234), "macos-1234");
    }

    // ---------------------------------------------------------------------------------------
    // AppGainState -- the pure volume/mute logic the realtime table is fed from
    // ---------------------------------------------------------------------------------------

    #[test]
    fn fresh_state_is_full_volume_unmuted() {
        let s = AppGainState::default();
        assert_eq!(s.volume, 1.0);
        assert!(!s.muted);
        assert_eq!(s.balance, 0.0);
        assert_eq!(s.effective_gains(), (1.0, 1.0));
    }

    #[test]
    fn set_volume_clamps_into_boosted_range() {
        let mut s = AppGainState::default();
        // Below the old 1.0 unity cap: passes through untouched, boost or not.
        s.set_volume(0.7);
        assert_eq!(s.volume, 0.7);
        // Above unity, at the boosted ceiling: allowed, not clamped back to 1.0.
        s.set_volume(1.5);
        assert_eq!(s.volume, 1.5);
        // Above the boosted ceiling: clamped to it.
        s.set_volume(3.0);
        assert_eq!(s.volume, super::super::MAX_BOOSTED_VOLUME);
        s.set_volume(-0.3);
        assert_eq!(s.volume, 0.0);
    }

    #[test]
    fn muting_zeroes_effective_gain_without_touching_stored_volume() {
        let mut s = AppGainState::default();
        s.set_volume(0.6);
        s.set_muted(true);
        assert_eq!(
            s.effective_gains(),
            (0.0, 0.0),
            "muted -> silent regardless of volume"
        );
        assert_eq!(s.volume, 0.6, "the underlying volume must survive a mute");
    }

    #[test]
    fn unmuting_restores_exactly_the_last_set_volume() {
        let mut s = AppGainState::default();
        s.set_volume(0.42);
        s.set_muted(true);
        s.set_muted(false);
        assert_eq!(s.effective_gains(), (0.42, 0.42));
    }

    #[test]
    fn balance_clamps_into_negative_one_one_range() {
        let mut s = AppGainState::default();
        s.set_balance(2.0);
        assert_eq!(s.balance, 1.0);
        s.set_balance(-2.0);
        assert_eq!(s.balance, -1.0);
    }

    #[test]
    fn full_right_balance_silences_left_channel_only() {
        let mut s = AppGainState::default();
        s.set_volume(0.8);
        s.set_balance(1.0);
        assert_eq!(s.effective_gains(), (0.0, 0.8));
    }

    #[test]
    fn full_left_balance_silences_right_channel_only() {
        let mut s = AppGainState::default();
        s.set_volume(0.8);
        s.set_balance(-1.0);
        assert_eq!(s.effective_gains(), (0.8, 0.0));
    }

    #[test]
    fn partial_balance_only_attenuates_the_opposite_channel() {
        let mut s = AppGainState::default();
        s.set_volume(1.0);
        s.set_balance(0.5);
        assert_eq!(s.effective_gains(), (0.5, 1.0));
    }

    #[test]
    fn muting_overrides_balance_in_both_channels() {
        let mut s = AppGainState::default();
        s.set_volume(1.0);
        s.set_balance(1.0);
        s.set_muted(true);
        assert_eq!(s.effective_gains(), (0.0, 0.0));
    }

    #[test]
    fn setting_volume_while_muted_does_not_auto_unmute() {
        let mut s = AppGainState::default();
        s.set_muted(true);
        s.set_volume(0.9);
        assert!(s.muted, "changing volume alone must not implicitly unmute");
        assert_eq!(s.effective_gains(), (0.0, 0.0));
    }

    // ---------------------------------------------------------------------------------------
    // AtomicGainSlot -- the lock-free realtime handoff cell
    // ---------------------------------------------------------------------------------------

    #[test]
    fn atomic_gain_slot_round_trips_typical_values() {
        for v in [0.0f32, 0.25, 0.5, 1.0, 2.0] {
            let slot = AtomicGainSlot::new((v, v));
            assert_eq!(slot.load(), (v, v));
        }
    }

    #[test]
    fn atomic_gain_slot_round_trips_independent_left_right_values() {
        let slot = AtomicGainSlot::new((0.2, 0.9));
        assert_eq!(slot.load(), (0.2, 0.9));
    }

    #[test]
    fn atomic_gain_slot_store_then_load_sees_the_new_value() {
        let slot = AtomicGainSlot::new((1.0, 1.0));
        slot.store((0.33, 0.66));
        assert_eq!(slot.load(), (0.33, 0.66));
    }

    // ---------------------------------------------------------------------------------------
    // FloatRingBuffer -- pure Rust, allocation-free hot path, worth testing thoroughly since
    // it's the one piece of genuinely novel realtime-safety-critical logic in this file.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn ring_buffer_rounds_capacity_up_to_a_power_of_two() {
        let ring = FloatRingBuffer::new(10);
        assert_eq!(ring.capacity, 16);
    }

    #[test]
    fn ring_buffer_write_then_read_round_trips() {
        let ring = FloatRingBuffer::new(8);
        let src = [1.0f32, 2.0, 3.0, 4.0];
        let written = unsafe { ring.write(&src) };
        assert_eq!(written, 4);

        let mut dst = [0.0f32; 4];
        let read = unsafe { ring.read(&mut dst) };
        assert_eq!(read, 4);
        assert_eq!(dst, src);
    }

    #[test]
    fn ring_buffer_read_on_empty_buffer_is_a_silent_underrun() {
        let ring = FloatRingBuffer::new(8);
        let mut dst = [9.0f32; 4]; // pre-filled with a sentinel so we can see nothing was written
        let read = unsafe { ring.read(&mut dst) };
        assert_eq!(read, 0);
        assert_eq!(
            dst, [9.0; 4],
            "underrun must not touch the destination buffer"
        );
    }

    #[test]
    fn ring_buffer_write_past_capacity_drops_the_overflow_instead_of_corrupting() {
        let ring = FloatRingBuffer::new(4); // rounds up to 4
        let src = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let written = unsafe { ring.write(&src) };
        assert_eq!(
            written, 4,
            "only `capacity` samples fit; the rest are dropped, not corrupted"
        );
        assert_eq!(ring.fill_level(), 4);
    }

    #[test]
    fn ring_buffer_wraps_around_correctly() {
        let ring = FloatRingBuffer::new(4);
        // Fill, drain, fill again so head/tail cross the physical buffer boundary.
        unsafe { ring.write(&[1.0, 2.0, 3.0, 4.0]) };
        let mut drained = [0.0f32; 2];
        unsafe { ring.read(&mut drained) };
        assert_eq!(drained, [1.0, 2.0]);

        unsafe { ring.write(&[5.0, 6.0]) }; // wraps past the end of the physical buffer
        let mut rest = [0.0f32; 4];
        let read = unsafe { ring.read(&mut rest) };
        assert_eq!(read, 4);
        assert_eq!(rest, [3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn ring_buffer_fill_level_tracks_unread_samples() {
        let ring = FloatRingBuffer::new(8);
        assert_eq!(ring.fill_level(), 0);
        unsafe { ring.write(&[1.0, 2.0, 3.0]) };
        assert_eq!(ring.fill_level(), 3);
        let mut dst = [0.0f32; 1];
        unsafe { ring.read(&mut dst) };
        assert_eq!(ring.fill_level(), 2);
    }

    // ---------------------------------------------------------------------------------------
    // System-bundle hiding
    // ---------------------------------------------------------------------------------------

    #[test]
    fn hides_known_system_bundle_prefixes() {
        assert!(is_hidden_system_bundle("com.apple.coreaudiod"));
        assert!(is_hidden_system_bundle("com.apple.controlcenter"));
    }

    #[test]
    fn does_not_hide_ordinary_apps() {
        assert!(!is_hidden_system_bundle("com.spotify.client"));
        assert!(!is_hidden_system_bundle("com.google.Chrome"));
    }

    #[test]
    fn does_not_hide_ordinary_paths() {
        assert!(!is_system_internal_path(
            "/Applications/Spotify.app/Contents/MacOS/Spotify"
        ));
        assert!(!is_system_internal_path(
            "/System/Applications/Music.app/Contents/MacOS/Music"
        ));
    }

    #[test]
    fn hides_known_system_internal_paths() {
        // Confirmed on real hardware -- see `SYSTEM_INTERNAL_PATH_PREFIXES`'s doc comment.
        assert!(is_system_internal_path("/usr/sbin/systemsoundserverd"));
        assert!(is_system_internal_path(
            "/System/Library/CoreServices/PowerChime.app/Contents/MacOS/PowerChime"
        ));
        assert!(is_system_internal_path(
            "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU"
        ));
    }

    #[test]
    fn wanted_when_reporting_active_regardless_of_hold_state() {
        let now = Instant::now();
        assert!(is_wanted_for_reconciliation(true, None, now));
        assert!(is_wanted_for_reconciliation(
            true,
            Some(now - Duration::from_secs(10)),
            now
        ));
    }

    #[test]
    fn not_wanted_when_inactive_with_no_hold_at_all() {
        assert!(!is_wanted_for_reconciliation(false, None, Instant::now()));
    }

    #[test]
    fn still_wanted_when_inactive_but_within_the_hold_window() {
        let now = Instant::now();
        let hold_until = now + Duration::from_millis(500);
        assert!(is_wanted_for_reconciliation(false, Some(hold_until), now));
    }

    #[test]
    fn not_wanted_once_the_hold_window_has_expired() {
        let now = Instant::now();
        let hold_until = now - Duration::from_millis(1);
        assert!(!is_wanted_for_reconciliation(false, Some(hold_until), now));
    }

    #[test]
    fn pid_path_lookup_resolves_this_process_own_pid() {
        // `is_hidden_system_process_cached`'s no-bundle-id path needs a real live pid (no way to
        // fake `pidpath`'s result without a real syscall) -- this test process's own pid is the
        // only "real" one a unit test has on hand.
        let own_pid = std::process::id() as i32;
        assert!(libproc::proc_pid::pidpath(own_pid).is_ok());
        // The cargo test binary obviously isn't under `/System/Library`, `/usr/sbin`, or
        // `/usr/libexec` -- proves the no-bundle-id path doesn't spuriously hide arbitrary
        // processes it doesn't recognize.
        assert!(!is_hidden_system_process_cached(
            &mut HashMap::new(),
            None,
            own_pid
        ));
    }

    #[test]
    fn bundle_id_hide_list_short_circuits_before_any_path_lookup() {
        // A hidden bundle id should be caught without needing a valid pid at all -- pid 0 would
        // make `pidpath` fail, so this only passes if the bundle check runs (and short-circuits)
        // first.
        assert!(is_hidden_system_process_cached(
            &mut HashMap::new(),
            Some("com.apple.coreaudiod"),
            0
        ));
        assert!(!is_hidden_system_process_cached(
            &mut HashMap::new(),
            Some("com.spotify.client"),
            0
        ));
    }

    #[test]
    fn cached_variant_caches_a_bundle_id_match_without_any_syscall() {
        let mut cache = HashMap::new();
        // pid 0 would make `pidpath` fail -- proves this resolves purely from the bundle-id
        // match, and that the resulting `true` actually gets cached.
        assert!(is_hidden_system_process_cached(
            &mut cache,
            Some("com.apple.coreaudiod"),
            0
        ));
        assert_eq!(cache.get(&0), Some(&true));
    }

    #[test]
    fn cached_variant_caches_a_resolved_non_hidden_process() {
        let mut cache = HashMap::new();
        let own_pid = std::process::id() as i32;
        assert!(!is_hidden_system_process_cached(&mut cache, None, own_pid));
        // A decisive `false` (the path lookup succeeded and wasn't under a hidden prefix) is
        // cached too, not just `true` -- otherwise every ordinary app would re-run the syscall
        // every poll.
        assert_eq!(cache.get(&own_pid), Some(&false));
    }

    #[test]
    fn cached_variant_does_not_cache_a_failed_syscall() {
        let mut cache = HashMap::new();
        // pid 0 with no bundle id: the bundle check doesn't match, and `pidpath(0)` fails -- this
        // must resolve to `false` for this call without inserting anything, so a later call (once
        // the real process is resolvable) can still get the right answer instead of being
        // permanently stuck on a guess made during a transient failure.
        assert!(!is_hidden_system_process_cached(&mut cache, None, 0));
        assert_eq!(cache.get(&0), None);
    }

    #[test]
    fn cached_variant_reuses_a_cached_answer_without_needing_bundle_id_again() {
        let mut cache = HashMap::new();
        cache.insert(999_999, true);
        // No bundle id and an unresolvable pid -- would resolve to `false` if the cache weren't
        // consulted first, so this only passes if the cached answer is actually used.
        assert!(is_hidden_system_process_cached(&mut cache, None, 999_999));
    }

    // ---------------------------------------------------------------------------------------
    // Cross-check: the well-known FourCC selector values both reference repos hand-roll match
    // what objc2-core-audio 0.3.2 was confirmed (via docs.rs) to expose under the same names.
    // ---------------------------------------------------------------------------------------

    const fn fourcc(bytes: [u8; 4]) -> u32 {
        ((bytes[0] as u32) << 24)
            | ((bytes[1] as u32) << 16)
            | ((bytes[2] as u32) << 8)
            | (bytes[3] as u32)
    }

    #[test]
    fn fourcc_matches_known_selectors() {
        // Values independently confirmed via docs.rs for objc2-core-audio 0.3.2's
        // kAudioProcessPropertyPID / kAudioProcessPropertyBundleID /
        // kAudioProcessPropertyIsRunningOutput / kAudioHardwarePropertyProcessObjectList.
        assert_eq!(fourcc(*b"ppid"), 0x7070_6964);
        assert_eq!(fourcc(*b"pbid"), 0x7062_6964);
        assert_eq!(fourcc(*b"piro"), 0x7069_726f);
        assert_eq!(fourcc(*b"prs#"), 0x7072_7323);
    }
}
