//! Platform-agnostic half of auto-duck: voice-activity classification and the debounce/hysteresis
//! state machine that decides when an app is "actually talking" for long enough to trigger a duck.
//!
//! This has zero platform-specific code (just `webrtc_vad` + atomics) -- pulled out of
//! `macos_ducking.rs` into its own module specifically so Windows' ducking backend
//! (`windows_ducking.rs`) can reuse the exact same classification/debounce behavior instead of
//! reimplementing it. What stays behind in `macos_ducking.rs` (`DuckingRuntime`'s
//! `UnsafeCell`-based single-owner-callback discipline, the on-disk settings path) is genuinely
//! macOS-specific: it exists only because Core Audio forces every tapped app's audio through one
//! shared realtime callback. Windows' per-process loopback capture gives one independent capture
//! stream per app instead, so it doesn't need that machinery at all -- each capture thread just
//! owns a plain [`SpeechDetector`] directly.
//!
//! See `macos_ducking.rs`'s original module doc comment (still there) for why this wraps
//! `webrtc-vad` rather than a hand-rolled classifier: real-world logging showed a from-scratch
//! energy/zero-crossing heuristic classified ordinary music as speech almost continuously.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use webrtc_vad::{SampleRate, Vad, VadMode};

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
pub const VAD_FRAME_SAMPLES: usize = 480;
pub const VAD_SAMPLE_RATE: SampleRate = SampleRate::Rate48kHz;
/// Same rate as [`VAD_SAMPLE_RATE`], as a plain number -- `webrtc_vad::SampleRate` is an enum
/// `Vad::new_with_rate_and_mode` wants, not something arithmetic (like resampling math) can use
/// directly. Only Windows' capture path (`windows_ducking.rs`) actually reads this -- it captures
/// at whatever rate WASAPI hands it and has to resample to this before the VAD will accept it,
/// where macOS's Core Audio taps are already configured to deliver 48kHz directly. `#[allow]`
/// rather than `#[cfg(target_os = "windows")]`: this is a real, intentional part of the shared
/// module's public surface, just not every platform's ducking backend needs it.
#[allow(dead_code)]
pub const VAD_SAMPLE_RATE_HZ: u32 = 48_000;

/// How much a ducked app's gain is multiplied by while something else is triggering. Not zero --
/// an instant full mute is jarring; a partial duck feels like the natural "someone started
/// talking" dip real mixing consoles do.
pub const DUCK_GAIN_MULTIPLIER: f32 = 0.22;

/// A [`SpeechDetector`]'s hysteresis counters, held as real atomics rather than plain fields --
/// deliberately the *only* part of a detector's state that's safe to read from a thread other
/// than the one that owns it. See [`Self::observe`]'s doc comment for the single-writer,
/// multi-reader access pattern this is built for.
///
/// On macOS, the reason this needs to be readable off the realtime thread at all: without it, an
/// engine rebuild -- which happens on every single app-start/stop -- would throw the detector
/// away and restart its "how long has this been speech" counter from zero. Confirmed live as a
/// real bug, not a hypothetical: a WhatsApp voice note starting is itself exactly the kind of
/// event that triggers a rebuild, resetting the very detector meant to catch it.
/// `MacosMixerBackend::reconcile_engine` snapshots these atomics before building a new engine and
/// seeds the new detectors from that snapshot instead of starting at zero. Windows' capture
/// threads are far longer-lived (one per priority app, torn down only when the app stops
/// producing sound or is removed from the priority list) so this cross-rebuild survival matters
/// less there, but the same atomics still make [`Self::snapshot`] cheap to read from
/// `WindowsMixerBackend::list_sessions`'s polling thread without any lock.
#[derive(Default)]
pub struct HysteresisCounters {
    speech_run: AtomicU32,
    silence_run: AtomicU32,
    is_triggering: AtomicBool,
}

impl HysteresisCounters {
    pub fn seeded(state: PersistedDuckState) -> Self {
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
    /// Relaxed everywhere: only a single capture thread (the realtime Core Audio callback on
    /// macOS, one dedicated OS thread per app on Windows) ever calls this for a given instance --
    /// the counters are atomic so [`Self::snapshot`] can safely read them from a different thread
    /// (a control/poll thread), not because there's a multi-writer race here to guard against.
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

/// Per-app speech-detection state: `webrtc-vad`'s own internal filter state, a small sample
/// accumulator bridging whatever chunk size the caller feeds in to VAD's rigid 10/20/30ms frame
/// requirement, and this app's [`HysteresisCounters`]. On macOS, everything except the
/// `Arc<HysteresisCounters>` handle is owned exclusively by the single realtime capture callback
/// (see `DuckingRuntime`'s doc comment in `macos_ducking.rs`); on Windows, a `SpeechDetector` is
/// owned entirely and directly by its one dedicated capture thread -- no `Arc`/`UnsafeCell`
/// needed there at all, since Windows never shares one detector across multiple callbacks the way
/// a Core Audio aggregate device forces.
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

    /// Feeds one callback's/chunk's worth of mono samples (already downmixed and, on Windows,
    /// already resampled to [`VAD_SAMPLE_RATE`] -- see `windows_ducking.rs`) through the VAD
    /// (accumulating into `pending` and classifying every complete `VAD_FRAME_SAMPLES`-length
    /// chunk that becomes available -- a call can complete zero, one, or more than one, depending
    /// on how its size lines up with the 480-sample frame), updates the debounced trigger state,
    /// and returns whether this app should currently be treated as a duck *trigger* -- always
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
        // A realistic callback/chunk size (not necessarily a multiple of VAD_FRAME_SAMPLES) --
        // exercises the accumulator's carry-over path, not just the exactly-480-samples-per-call
        // happy path.
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
