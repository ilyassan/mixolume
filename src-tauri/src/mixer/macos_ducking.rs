//! macOS-specific half of auto-duck: the realtime-callback-shaped runtime (`DuckingRuntime`) and
//! on-disk settings persistence. The actual voice-activity classification/debounce logic
//! (`SpeechDetector`, `HysteresisCounters`, `PersistedDuckState`) used to live here too but is
//! now in `duck_detect.rs`, shared with Windows' ducking backend -- see that module's doc comment
//! for why, and for the original rationale on wrapping `webrtc-vad` instead of a from-scratch
//! classifier (real-world logging showed a hand-rolled heuristic classified ordinary music as
//! speech almost continuously).
//!
//! What's still genuinely macOS-only here: `DuckingRuntime`'s `UnsafeCell`-based single-owner
//! discipline exists only because Core Audio forces every tapped app's audio through one shared
//! realtime callback -- Windows' per-process loopback capture gives one independent stream per
//! app instead, so its capture threads just own a `SpeechDetector` directly (see
//! `windows_ducking.rs`), no shared-callback machinery needed.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::DuckingSettings;

// Re-exported (not just `use`d) so every existing `macos_ducking::PersistedDuckState`/
// `macos_ducking::DUCK_GAIN_MULTIPLIER`/etc. reference throughout `macos.rs` keeps working
// unchanged after this module's split -- these are also what the rest of this file uses directly.
pub use super::duck_detect::{HysteresisCounters, PersistedDuckState, SpeechDetector, DUCK_GAIN_MULTIPLIER};

/// How much a callback nudges a ducked/restoring app's *actual* applied gain multiplier toward
/// its target (0.0 or [`DUCK_GAIN_MULTIPLIER`]) each callback, rather than snapping instantly --
/// an abrupt gain jump is exactly the kind of click this project already spent real effort
/// eliminating elsewhere (see the macOS hiccup fixes). At a typical ~10ms callback this reaches
/// roughly 90% of the way to the target within ~150ms: a natural-feeling dip, not a lag.
const DUCK_SMOOTHING_PER_CALLBACK: f32 = 0.15;

/// Per-engine-instance realtime ducking state: one [`SpeechDetector`] and one smoothed gain
/// multiplier per currently-tapped app, plus the master on/off switch. Owned exclusively by the
/// single realtime capture callback -- same single-owner discipline `Scratch` documents in
/// `macos.rs`, just covering two parallel `Vec`s instead of one flat sample buffer.
///
/// `enabled` is deliberately the one piece of state shared *across* engine rebuilds (via the
/// `Arc` a caller clones in from `Inner`) rather than baked in at construction time like the
/// per-app exclusion list is -- toggling the feature off in Settings should take effect on the
/// very next audio callback, not wait for some app to start/stop and trigger a rebuild.
pub struct DuckingRuntime {
    enabled: Arc<AtomicBool>,
    detectors: UnsafeCell<Vec<SpeechDetector>>,
    multipliers: UnsafeCell<Vec<f32>>,
    /// One per tap, same order/index as `detectors` -- kept as its own plain (non-`UnsafeCell`)
    /// `Vec` of `Arc`s specifically so [`Self::snapshot_all`] can read it with ordinary safe Rust
    /// from the control thread, no unsafe accessor needed. See [`HysteresisCounters`]'s doc
    /// comment for why this exists at all.
    hysteresis: Vec<Arc<HysteresisCounters>>,
}

impl DuckingRuntime {
    /// `per_tap_excluded`/`per_tap_persisted` must be the same length, one entry per tap, in the
    /// same order the caller will index taps by everywhere else (matching `gain_slots`).
    pub fn new(
        enabled: Arc<AtomicBool>,
        per_tap_excluded: Vec<bool>,
        per_tap_persisted: Vec<PersistedDuckState>,
    ) -> Self {
        let count = per_tap_excluded.len();
        let hysteresis: Vec<Arc<HysteresisCounters>> = per_tap_persisted
            .into_iter()
            .map(|state| Arc::new(HysteresisCounters::seeded(state)))
            .collect();
        let detectors = per_tap_excluded
            .into_iter()
            .zip(hysteresis.iter().cloned())
            .map(|(excluded, h)| SpeechDetector::new(excluded, h))
            .collect();
        Self {
            enabled,
            detectors: UnsafeCell::new(detectors),
            multipliers: UnsafeCell::new(vec![1.0; count]),
            hysteresis,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// # Safety
    /// Caller must be the single realtime capture callback that owns this `DuckingRuntime` (see
    /// struct doc comment) -- never call from more than one thread/closure for the same value.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn detectors_mut(&self) -> &mut Vec<SpeechDetector> {
        &mut *self.detectors.get()
    }

    /// # Safety
    /// Same contract as [`Self::detectors_mut`].
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn multipliers_mut(&self) -> &mut Vec<f32> {
        &mut *self.multipliers.get()
    }

    /// Safe to call from any thread, at any time -- see [`HysteresisCounters`]'s doc comment.
    /// One entry per tap, same order as everywhere else.
    pub fn snapshot_all(&self) -> Vec<PersistedDuckState> {
        self.hysteresis.iter().map(|h| h.snapshot()).collect()
    }

    pub const SMOOTHING_PER_CALLBACK: f32 = DUCK_SMOOTHING_PER_CALLBACK;
}

// SAFETY: see the struct doc comment -- single-owning-callback discipline, not real
// synchronization. Only ever shared as `Arc<DuckingRuntime>` with exactly one realtime thread
// calling `detectors_mut`/`multipliers_mut`.
unsafe impl Sync for DuckingRuntime {}
unsafe impl Send for DuckingRuntime {}

fn config_file_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support/MiXolume/ducking-config.json"),
    )
}

/// Loads persisted settings from disk, or the default (disabled, nothing excluded) if none have
/// ever been saved, the file is unreadable, or `$HOME` can't be resolved -- a missing/corrupt
/// config should never stop the app from starting.
pub fn load_settings() -> DuckingSettings {
    config_file_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &DuckingSettings) {
    let Some(path) = config_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

// All classification/debounce unit tests moved to `duck_detect.rs` along with the types they
// test. Nothing left in this file (`DuckingRuntime`'s realtime-callback wiring, on-disk settings
// I/O) is cheaply unit-testable without a real Core Audio callback or filesystem, matching why
// this file never had tests of its own beyond those.
