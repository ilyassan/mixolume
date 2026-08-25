//! Lightweight, model-free voice-activity detection driving cross-app "auto-duck" (lower
//! everything else while one app is producing real speech -- an incoming call, a WhatsApp voice
//! note, dialogue in a video). Deliberately not a machine-learning classifier: this runs inline
//! in the realtime audio callback (see `macos.rs`'s `mix_capture_callback`), so it needs to be
//! unconditionally cheap and allocation-free, not just "fast enough on average." The features
//! used below -- short-term energy, zero-crossing rate, and a crude speech-band energy ratio
//! from two cascaded one-pole filters -- are the classic, decades-old building blocks of
//! energy/ZCR-based voice activity detection. Not novel, but well-understood, unconditionally
//! stable, and correctly free of any ML runtime/model-file dependency this project doesn't
//! otherwise need.
//!
//! Known limitation, stated plainly rather than glossed over: this cannot reliably distinguish
//! speech from vocal-heavy music -- singing occupies similar energy/ZCR/band-ratio ranges to
//! talking. A real speech classifier (e.g. a small model like Silero VAD) would do meaningfully
//! better there. Treated as an acceptable v1 tradeoff: the alternative pulls in a model file and
//! an inference runtime for a feature whose entire selling point is that it comes essentially
//! free on top of a mixer this project already needed to build for its own sake. The
//! thresholds below are reasoned starting points, not independently validated against a real
//! speech/music corpus -- expect to tune them once this has run against real audio.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::DuckingSettings;

/// Below this normalized RMS, a callback's audio is treated as silence outright -- skips even
/// running the zero-crossing/band-ratio math (meaningless on noise-floor-level signal anyway,
/// and would otherwise flicker the detector right at the edge of true silence).
const SILENCE_RMS_FLOOR: f32 = 0.01;

/// Zero-crossing rate (fraction of samples that flip sign) speech typically falls within. Pure
/// sustained tones/bass sit below this; broadband noise/cymbals/hi-hats sit above it.
const ZCR_MIN: f32 = 0.02;
const ZCR_MAX: f32 = 0.25;

/// A frame's speech-band (roughly telephone bandwidth, 300Hz-3400Hz) energy must be at least
/// this fraction of its total energy to count as speech-like.
const BAND_RATIO_MIN: f32 = 0.15;

/// Consecutive speech-classified callbacks required before actually triggering a duck --
/// debounces a single stray frame (one loud consonant, a short notification blip) from yanking
/// everyone else's volume down. At a typical ~10ms Core Audio callback size this is roughly
/// 400ms of continuous speech: closer to "someone's actually talking" than "one syllable
/// happened."
const TRIGGER_ON_FRAMES: u32 = 40;

/// Consecutive non-speech callbacks required before releasing a duck -- deliberately longer than
/// the trigger threshold, so natural pauses/breaths mid-sentence don't flicker the volume back up
/// and down. Roughly 1.5s at ~10ms callbacks.
const TRIGGER_OFF_FRAMES: u32 = 150;

/// Assumed sample rate for the speech-band filter's cutoff frequencies. Not read from the real
/// device -- that would mean threading the aggregate's actual nominal sample rate through just
/// for this heuristic. 48kHz is what the overwhelming majority of macOS output devices run at,
/// and a VAD heuristic doesn't need the precision that would matter for real audio processing.
const ASSUMED_SAMPLE_RATE: f32 = 48_000.0;

const HIGHPASS_CUTOFF_HZ: f32 = 300.0;
const LOWPASS_CUTOFF_HZ: f32 = 3_400.0;

/// How much a ducked app's gain is multiplied by while something else is triggering. Not zero --
/// an instant full mute is jarring; a partial duck feels like the natural "someone started
/// talking" dip real mixing consoles do.
pub const DUCK_GAIN_MULTIPLIER: f32 = 0.22;

fn one_pole_lowpass_alpha(cutoff_hz: f32, sample_rate: f32) -> f32 {
    let dt = 1.0 / sample_rate;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    dt / (rc + dt)
}

fn one_pole_highpass_alpha(cutoff_hz: f32, sample_rate: f32) -> f32 {
    let dt = 1.0 / sample_rate;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    rc / (rc + dt)
}

/// Per-app realtime state: the cascaded one-pole filters' memory and the debounced
/// trigger/release hysteresis. Owned exclusively by the single realtime capture callback -- same
/// single-owner discipline `Scratch` documents elsewhere in `macos.rs`, just not sharing that
/// exact type since this isn't a flat sample buffer.
pub struct SpeechDetector {
    excluded_from_triggering: bool,
    hp_alpha: f32,
    lp_alpha: f32,
    hp_prev_in: f32,
    hp_prev_out: f32,
    lp_prev_out: f32,
    prev_sample_positive: bool,
    has_prev_sample: bool,
    speech_run: u32,
    silence_run: u32,
    is_triggering: bool,
}

impl SpeechDetector {
    pub fn new(excluded_from_triggering: bool) -> Self {
        Self {
            excluded_from_triggering,
            hp_alpha: one_pole_highpass_alpha(HIGHPASS_CUTOFF_HZ, ASSUMED_SAMPLE_RATE),
            lp_alpha: one_pole_lowpass_alpha(LOWPASS_CUTOFF_HZ, ASSUMED_SAMPLE_RATE),
            hp_prev_in: 0.0,
            hp_prev_out: 0.0,
            lp_prev_out: 0.0,
            prev_sample_positive: true,
            has_prev_sample: false,
            speech_run: 0,
            silence_run: 0,
            is_triggering: false,
        }
    }

    /// Feeds one callback's worth of mono-summed samples through the filters/feature
    /// accumulators, updates the debounced trigger state, and returns whether this app should
    /// currently be treated as a duck *trigger* -- always `false` if excluded, regardless of
    /// what the audio actually sounds like, without even running the classification math.
    pub fn process_frame(&mut self, mono_samples: &[f32]) -> bool {
        if self.excluded_from_triggering || mono_samples.is_empty() {
            return false;
        }

        let mut total_energy = 0.0f32;
        let mut band_energy = 0.0f32;
        let mut zero_crossings = 0u32;

        for &x in mono_samples {
            total_energy += x * x;

            // Cascaded one-pole highpass then lowpass -- a crude approximation of a
            // telephone-bandwidth bandpass filter. Cheap (a handful of flops per sample) and
            // unconditionally stable (one-pole filters with alpha in (0,1) can't blow up the
            // way a resonant biquad might), which matters more here than filter precision does.
            let hp_out = self.hp_alpha * (self.hp_prev_out + x - self.hp_prev_in);
            self.hp_prev_in = x;
            self.hp_prev_out = hp_out;

            self.lp_prev_out += self.lp_alpha * (hp_out - self.lp_prev_out);
            band_energy += self.lp_prev_out * self.lp_prev_out;

            let positive = x >= 0.0;
            if self.has_prev_sample && positive != self.prev_sample_positive {
                zero_crossings += 1;
            }
            self.prev_sample_positive = positive;
            self.has_prev_sample = true;
        }

        let n = mono_samples.len() as f32;
        let rms = (total_energy / n).sqrt();
        let zcr = zero_crossings as f32 / n;
        let band_ratio = if total_energy > 0.0 {
            band_energy / total_energy
        } else {
            0.0
        };

        let looks_like_speech = rms >= SILENCE_RMS_FLOOR
            && (ZCR_MIN..=ZCR_MAX).contains(&zcr)
            && band_ratio >= BAND_RATIO_MIN;

        if looks_like_speech {
            self.speech_run += 1;
            self.silence_run = 0;
        } else {
            self.silence_run += 1;
            self.speech_run = 0;
        }

        if !self.is_triggering && self.speech_run >= TRIGGER_ON_FRAMES {
            self.is_triggering = true;
        } else if self.is_triggering && self.silence_run >= TRIGGER_OFF_FRAMES {
            self.is_triggering = false;
        }

        self.is_triggering
    }

    /// The debounced trigger state as of the last [`Self::process_frame`] call, without
    /// re-running any classification.
    pub fn is_triggering(&self) -> bool {
        self.is_triggering
    }
}

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
}

impl DuckingRuntime {
    pub fn new(enabled: Arc<AtomicBool>, per_tap_excluded: Vec<bool>) -> Self {
        let count = per_tap_excluded.len();
        let detectors = per_tap_excluded
            .into_iter()
            .map(SpeechDetector::new)
            .collect();
        Self {
            enabled,
            detectors: UnsafeCell::new(detectors),
            multipliers: UnsafeCell::new(vec![1.0; count]),
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

    /// A pure low-frequency tone (well below the speech band, and with a zero-crossing rate far
    /// below `ZCR_MIN`) -- stands in for bass-heavy music, which should never trigger ducking.
    fn pure_tone(freq_hz: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let t = i as f32 / ASSUMED_SAMPLE_RATE;
                (2.0 * std::f32::consts::PI * freq_hz * t).sin() * 0.8
            })
            .collect()
    }

    #[test]
    fn silence_never_triggers_even_after_many_frames() {
        let mut detector = SpeechDetector::new(false);
        let frame = silence(480); // ~10ms at 48kHz
        for _ in 0..(TRIGGER_ON_FRAMES * 3) {
            assert!(!detector.process_frame(&frame));
        }
    }

    #[test]
    fn low_frequency_tone_never_triggers() {
        // 100Hz: analytically, zero-crossing rate = 2*100/48000 ≈ 0.0042, well under ZCR_MIN --
        // this is exactly the "sustained bass tone" case the ZCR check exists to reject.
        let mut detector = SpeechDetector::new(false);
        let frame = pure_tone(100.0, 480);
        for _ in 0..(TRIGGER_ON_FRAMES * 3) {
            assert!(!detector.process_frame(&frame));
        }
    }

    #[test]
    fn excluded_detector_never_triggers_regardless_of_content() {
        let mut detector = SpeechDetector::new(true);
        // A wide-open threshold-passing signal would still need real speech-like content to
        // trigger, but excluded detectors return `false` before ever inspecting it -- feed it
        // deliberately loud broadband-ish content and confirm exclusion wins regardless.
        let frame: Vec<f32> = (0..480)
            .map(|i| if i % 3 == 0 { 0.9 } else { -0.7 })
            .collect();
        for _ in 0..(TRIGGER_ON_FRAMES * 3) {
            assert!(!detector.process_frame(&frame));
        }
    }

    #[test]
    fn empty_frame_does_not_trigger_or_panic() {
        let mut detector = SpeechDetector::new(false);
        assert!(!detector.process_frame(&[]));
    }
}
