//! Voice-activity detection driving cross-app "auto-duck" (lower everything else while one app
//! is producing real speech -- an incoming call, a WhatsApp voice note, dialogue in a video).
//!
//! First shipped with a hand-rolled energy/zero-crossing-rate heuristic (see git history if
//! curious). Confirmed live on real hardware that it was **not good enough**: real-world
//! logging showed ordinary music classified as speech almost continuously (`looks_like_speech`
//! true for the overwhelming majority of frames while a song played), while an actual WhatsApp
//! voice note played *alongside* it barely registered. That data, not just a theoretical
//! limitation, is why this now wraps `webrtc-vad` (a safe Rust binding to libfvad, the actual
//! WebRTC project's Voice-Activity-Detection module) instead of a from-scratch classifier --
//! the same Gaussian-Mixture-Model code that decides when someone is talking in essentially
//! every WebRTC video call, not a novel invention, and not a bundled ML runtime/model file
//! either (see `Cargo.toml`'s dependency comment for the full tradeoff reasoning against
//! something like Silero VAD).
//!
//! What's still this project's own, hand-built logic, and was *not* the source of the false
//! positives: the debounce/hysteresis state machine ([`HysteresisCounters`]) and its
//! survival-across-engine-rebuilds design. That part was already correct -- see its doc comment
//! for the real, separately-confirmed bug it fixes.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use webrtc_vad::{SampleRate, Vad, VadMode};

use super::DuckingSettings;

/// A [`SpeechDetector`]'s hysteresis counters, as a plain snapshot -- what survives an engine
/// rebuild. See [`HysteresisCounters`]'s doc comment for why this exists at all.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PersistedDuckState {
    pub speech_run: u32,
    pub silence_run: u32,
    pub is_triggering: bool,
}

/// Consecutive speech-classified 10ms frames required before actually triggering a duck --
/// debounces a single stray frame (one loud consonant, a short notification blip) from yanking
/// everyone else's volume down. 40 frames is ~400ms of continuous speech: closer to "someone's
/// actually talking" than "one syllable happened."
const TRIGGER_ON_FRAMES: u32 = 40;

/// Consecutive non-speech frames required before releasing a duck -- deliberately longer than
/// the trigger threshold, so natural pauses/breaths mid-sentence don't flicker the volume back up
/// and down. 150 frames is ~1.5s.
const TRIGGER_OFF_FRAMES: u32 = 150;

/// `webrtc-vad` requires audio in exact 10/20/30ms chunks at a fixed sample rate -- 480 samples
/// (10ms at 48kHz) is the smallest valid frame, closest to this project's existing per-callback
/// granularity and the most responsive option.
const VAD_FRAME_SAMPLES: usize = 480;
const VAD_SAMPLE_RATE: SampleRate = SampleRate::Rate48kHz;

/// How much a ducked app's gain is multiplied by while something else is triggering. Not zero --
/// an instant full mute is jarring; a partial duck feels like the natural "someone started
/// talking" dip real mixing consoles do.
pub const DUCK_GAIN_MULTIPLIER: f32 = 0.22;

/// A [`SpeechDetector`]'s hysteresis counters, held as real atomics rather than plain fields --
/// deliberately the *only* part of a detector's state that's safe to read from a thread other
/// than the realtime capture callback that owns it. The VAD's own internal filter state stays
/// behind `DuckingRuntime`'s single-owner `UnsafeCell` discipline as before; this type exists as
/// a fully separate allocation on purpose, rather than atomic fields living alongside non-atomic
/// ones inside the same raw-pointer-accessed struct -- mixing those two access patterns on one
/// allocation is subtler undefined-behavior territory than this project wants to reason about.
/// Two clean, independently-safe pieces is simpler.
///
/// The reason this needs to be readable off the realtime thread at all: without it, an engine
/// rebuild -- which happens on every single app-start/stop (see `TapEngine`'s doc comment) --
/// would throw the detector away and restart its "how long has this been speech" counter from
/// zero. Confirmed live as a real bug, not a hypothetical: a WhatsApp voice note starting is
/// itself exactly the kind of event that triggers a rebuild, resetting the very detector meant
/// to catch it. `MacosMixerBackend::reconcile_engine` snapshots these atomics (via
/// [`DuckingRuntime::snapshot_all`]) from the outgoing engine before building the new one, and
/// seeds the new detectors from that snapshot instead of starting at zero.
#[derive(Default)]
pub struct HysteresisCounters {
    speech_run: AtomicU32,
    silence_run: AtomicU32,
    is_triggering: AtomicBool,
}

impl HysteresisCounters {
    fn seeded(state: PersistedDuckState) -> Self {
        Self {
            speech_run: AtomicU32::new(state.speech_run),
            silence_run: AtomicU32::new(state.silence_run),
            is_triggering: AtomicBool::new(state.is_triggering),
        }
    }

    /// Safe to call from any thread -- see the struct doc comment.
    pub fn snapshot(&self) -> PersistedDuckState {
        PersistedDuckState {
            speech_run: self.speech_run.load(Ordering::Relaxed),
            silence_run: self.silence_run.load(Ordering::Relaxed),
            is_triggering: self.is_triggering.load(Ordering::Relaxed),
        }
    }

    /// Feeds one classified 10ms frame's result into the debounce state machine and returns the
    /// (possibly just-updated) trigger state. Pulled out as its own method, independent of the
    /// VAD/audio plumbing around it, specifically so the debounce *logic* is unit-testable with
    /// a plain synthetic true/false sequence -- it doesn't need real or fake audio, and
    /// shouldn't: classification accuracy is now `webrtc-vad`'s responsibility, a real,
    /// independently-proven library this project doesn't need to re-verify.
    ///
    /// Relaxed everywhere: only the single realtime capture thread ever calls this -- the
    /// counters are atomic so [`Self::snapshot`] can safely read them from the control thread
    /// during a rebuild, not because there's a multi-writer race here to guard against.
    pub fn observe(&self, looks_like_speech: bool) -> bool {
        let (speech_run, silence_run) = if looks_like_speech {
            (self.speech_run.fetch_add(1, Ordering::Relaxed) + 1, {
                self.silence_run.store(0, Ordering::Relaxed);
                0
            })
        } else {
            (
                {
                    self.speech_run.store(0, Ordering::Relaxed);
                    0
                },
                self.silence_run.fetch_add(1, Ordering::Relaxed) + 1,
            )
        };

        let was_triggering = self.is_triggering.load(Ordering::Relaxed);
        let is_triggering = if !was_triggering && speech_run >= TRIGGER_ON_FRAMES {
            true
        } else if was_triggering && silence_run >= TRIGGER_OFF_FRAMES {
            false
        } else {
            was_triggering
        };
        if is_triggering != was_triggering {
            self.is_triggering.store(is_triggering, Ordering::Relaxed);
        }
        is_triggering
    }
}

/// Per-app realtime state: `webrtc-vad`'s own internal filter state, a small sample accumulator
/// bridging Core Audio's callback size to VAD's rigid 10/20/30ms frame requirement, and a handle
/// to this app's [`HysteresisCounters`] (which -- unlike everything else here -- survives an
/// engine rebuild, see that type's doc comment). Everything except the `Arc<HysteresisCounters>`
/// handle is owned exclusively by the single realtime capture callback -- same single-owner
/// discipline `Scratch` documents elsewhere in `macos.rs`, just not sharing that exact type
/// since this isn't a flat sample buffer.
pub struct SpeechDetector {
    excluded_from_triggering: bool,
    hysteresis: Arc<HysteresisCounters>,
    vad: Vad,
    /// Accumulates incoming mono samples (converted to i16) until there's enough for one exact
    /// `VAD_FRAME_SAMPLES`-length frame. Pre-sized with spare capacity in `new` and drained (not
    /// collected) in `process_frame` specifically so this never allocates on the realtime
    /// thread once warmed up.
    pending: Vec<i16>,
}

impl SpeechDetector {
    pub fn new(excluded_from_triggering: bool, hysteresis: Arc<HysteresisCounters>) -> Self {
        Self {
            excluded_from_triggering,
            hysteresis,
            // Aggressive mode: biases toward classifying uncertain/ambiguous content as
            // *non*-speech. Chosen deliberately, not left at the default -- the confirmed real
            // problem was false positives on music, not missed speech, so trading a little
            // sensitivity for fewer false triggers is the right direction to lean.
            vad: Vad::new_with_rate_and_mode(VAD_SAMPLE_RATE, VadMode::Aggressive),
            pending: Vec::with_capacity(VAD_FRAME_SAMPLES * 2),
        }
    }

    /// Feeds one callback's worth of mono-summed samples through the VAD (accumulating into
    /// `pending` and classifying every complete `VAD_FRAME_SAMPLES`-length chunk that becomes
    /// available -- a callback can complete zero, one, or more than one, depending on how its
    /// size lines up with the 480-sample frame), updates the debounced trigger state, and
    /// returns whether this app should currently be treated as a duck *trigger* -- always
    /// `false` if excluded, regardless of what the audio actually sounds like, without even
    /// touching the VAD.
    pub fn process_frame(&mut self, mono_samples: &[f32]) -> bool {
        if self.excluded_from_triggering || mono_samples.is_empty() {
            return false;
        }

        for &x in mono_samples {
            let clamped = x.clamp(-1.0, 1.0);
            self.pending.push((clamped * i16::MAX as f32) as i16);
        }

        while self.pending.len() >= VAD_FRAME_SAMPLES {
            let looks_like_speech = self
                .vad
                .is_voice_segment(&self.pending[..VAD_FRAME_SAMPLES])
                .unwrap_or(false);
            // No `.collect()` -- an in-place shift of the remaining samples, not an allocation.
            self.pending.drain(..VAD_FRAME_SAMPLES);

            self.hysteresis.observe(looks_like_speech);
        }

        self.hysteresis.is_triggering.load(Ordering::Relaxed)
    }

    /// The debounced trigger state as of the last [`Self::process_frame`] call, without
    /// re-running any classification.
    pub fn is_triggering(&self) -> bool {
        self.hysteresis.is_triggering.load(Ordering::Relaxed)
    }
}

/// How much a callback nudges a ducked/restoring app's *actual* applied gain multiplier toward
/// its target (0.0 or [`DUCK_GAIN_MULTIPLIER`]) each callback, rather than snapping instantly --
/// an abrupt gain jump is exactly the kind of click this project already spent real effort
/// eliminating elsewhere (see the macOS hiccup fixes). At a typical ~10ms callback this reaches
/// roughly 90% of the way to the target within ~350ms: still a natural, fast mixing-console-style
/// dip, not a lag -- deliberately not faster than that. A higher coefficient (this used to be
/// 0.15, ~90% within ~150ms) converged *faster than the UI's poll loop could observe*: confirmed
/// live that almost the entire visual transition had already happened invisibly by the time the
/// frontend got its very first snapshot of it, which no amount of client-side animation can
/// retroactively make look gradual. `DUCK_TRANSITION_POLL_INTERVAL` in `lib.rs` polls faster
/// specifically while a duck is active, but that only helps if there's still a real ramp in
/// progress to sample.
const DUCK_SMOOTHING_PER_CALLBACK: f32 = 0.065;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn silence(len: usize) -> Vec<f32> {
        vec![0.0; len]
    }

    fn fresh_hysteresis() -> Arc<HysteresisCounters> {
        Arc::new(HysteresisCounters::seeded(PersistedDuckState::default()))
    }

    // ---------------------------------------------------------------------------------------
    // HysteresisCounters::observe -- the debounce state machine, tested directly against a
    // synthetic true/false sequence. This is deliberately *not* audio-based: classification
    // accuracy is `webrtc-vad`'s responsibility now (a real, independently-proven library this
    // project doesn't need to re-verify), so what's actually worth unit-testing here is this
    // project's own logic -- does the trigger/release debounce behave correctly given a known
    // sequence of speech/non-speech frames.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn does_not_trigger_before_enough_consecutive_speech_frames() {
        let h = fresh_hysteresis();
        for _ in 0..(TRIGGER_ON_FRAMES - 1) {
            assert!(!h.observe(true));
        }
    }

    #[test]
    fn triggers_exactly_at_the_threshold() {
        let h = fresh_hysteresis();
        for _ in 0..(TRIGGER_ON_FRAMES - 1) {
            h.observe(true);
        }
        assert!(h.observe(true));
    }

    #[test]
    fn a_single_non_speech_frame_resets_the_speech_run() {
        let h = fresh_hysteresis();
        for _ in 0..(TRIGGER_ON_FRAMES - 1) {
            h.observe(true);
        }
        assert!(!h.observe(false)); // one non-speech frame right before the threshold
        for _ in 0..(TRIGGER_ON_FRAMES - 1) {
            assert!(!h.observe(true)); // needs the full run over again
        }
        assert!(h.observe(true));
    }

    #[test]
    fn stays_triggered_through_brief_gaps_shorter_than_the_release_threshold() {
        let h = fresh_hysteresis();
        for _ in 0..TRIGGER_ON_FRAMES {
            h.observe(true);
        }
        assert!(h.is_triggering.load(Ordering::Relaxed));
        for _ in 0..(TRIGGER_OFF_FRAMES - 1) {
            assert!(h.observe(false), "should still be triggering mid-gap");
        }
        assert!(!h.observe(false), "releases once the gap is long enough");
    }

    #[test]
    fn seeding_from_a_persisted_snapshot_preserves_progress_across_a_simulated_rebuild() {
        let h = fresh_hysteresis();
        for _ in 0..(TRIGGER_ON_FRAMES - 3) {
            assert!(!h.observe(true));
        }
        let snapshot = h.snapshot();
        assert_eq!(snapshot.speech_run, TRIGGER_ON_FRAMES - 3);
        assert!(!snapshot.is_triggering);

        // A rebuild replaces the whole detector (including the VAD's own filter state) but
        // seeds the new one's hysteresis from that snapshot, the same way `TapEngine::new` does
        // via `DuckingRuntime::new`'s `per_tap_persisted` parameter.
        let post_rebuild = HysteresisCounters::seeded(snapshot);
        let mut triggered = false;
        for _ in 0..3 {
            triggered = post_rebuild.observe(true);
        }
        assert!(
            triggered,
            "only the remaining 3 frames should be needed, not all 40 over again"
        );
    }

    // ---------------------------------------------------------------------------------------
    // SpeechDetector -- just the plumbing (exclusion, empty input, the frame accumulator not
    // panicking or misbehaving on real-shaped input). Not re-testing VAD classification
    // accuracy itself here, for the same reason noted above.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn excluded_detector_never_triggers_regardless_of_content() {
        let mut detector = SpeechDetector::new(true, fresh_hysteresis());
        let frame: Vec<f32> = (0..480)
            .map(|i| if i % 3 == 0 { 0.9 } else { -0.7 })
            .collect();
        for _ in 0..(TRIGGER_ON_FRAMES * 3) {
            assert!(!detector.process_frame(&frame));
        }
    }

    #[test]
    fn empty_frame_does_not_trigger_or_panic() {
        let mut detector = SpeechDetector::new(false, fresh_hysteresis());
        assert!(!detector.process_frame(&[]));
    }

    #[test]
    fn silence_never_triggers_even_after_many_callbacks() {
        let mut detector = SpeechDetector::new(false, fresh_hysteresis());
        // A realistic Core Audio callback size (not necessarily a multiple of
        // VAD_FRAME_SAMPLES) -- exercises the accumulator's carry-over path, not just the
        // exactly-480-samples-per-call happy path.
        let frame = silence(512);
        for _ in 0..(TRIGGER_ON_FRAMES * 3) {
            assert!(!detector.process_frame(&frame));
        }
    }

    #[test]
    fn accumulator_handles_callback_sizes_smaller_than_one_vad_frame() {
        let mut detector = SpeechDetector::new(false, fresh_hysteresis());
        // Feeding tiny chunks (well under VAD_FRAME_SAMPLES) repeatedly should accumulate and
        // classify correctly instead of panicking or silently dropping samples.
        let tiny_frame = silence(37);
        for _ in 0..2000 {
            detector.process_frame(&tiny_frame);
        }
    }
}
