//! Windows backend for auto-duck: WASAPI "Process Loopback Capture" -- an undocumented-in-the-
//! sense-of-obscure-but-official Microsoft API (the same one OBS uses for per-application audio
//! capture) that lets a specific process's (and its child processes') rendered audio be captured
//! directly, independent of whatever else is playing on the default output device. Confirmed
//! working end-to-end on real hardware via a standalone proof-of-concept before this file was
//! written: activation, capture, and feeding `webrtc-vad` (see `duck_detect.rs`) all ran cleanly
//! against a real distinct process.
//!
//! Architecturally simpler than macOS's `TapEngine`/`CaptureAggregate` (`macos.rs`): Core Audio's
//! process-tap API only works by rerouting *all* tapped apps' audio through one shared aggregate
//! device and one realtime IOProc callback, which is why that side needs the `UnsafeCell`/
//! single-owner-callback machinery in `macos_ducking.rs`'s `DuckingRuntime`. Windows' per-process
//! loopback capture instead gives one fully independent `IAudioClient` stream per targeted
//! process -- so this file just spawns one plain OS thread per priority-trigger app, each owning
//! its own `duck_detect::SpeechDetector` directly, with no shared realtime state at all beyond a
//! single `Arc<AtomicBool>` publishing whether that app is currently triggering.
//!
//! Unlike macOS (which builds its own gain-control pipeline from scratch and never touches a real
//! OS volume knob), Windows ducking has to *apply* the duck by writing through the same
//! `ISimpleAudioVolume` this crate's normal volume control already uses (see `windows.rs`) --
//! there's no other volume control to hook into. That's `windows.rs`'s job, not this file's; this
//! file only ever answers "is this app talking right now."
//!
//! The COM activation dance and capture-format workaround are shared with volume boost's own
//! capture half (`windows_boost.rs`) -- see `windows_audio.rs`, which both of these build on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Media::Audio::{
    IAudioCaptureClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::duck_detect::{HysteresisCounters, SpeechDetector, VAD_SAMPLE_RATE_HZ};
use super::windows_audio::{
    activate_process_loopback_client, hardcoded_capture_format, CaptureFormat, LinearResampler,
};
use super::DuckingSettings;

// ==================================================================================================
// Mono downmix, specific to VAD's needs (boost's own render path needs the opposite -- preserve
// channels -- so this stays here rather than moving to the shared `windows_audio` module).
// ==================================================================================================

/// Downmixes one packet's worth of interleaved raw bytes into mono `f32` samples in roughly
/// [-1.0, 1.0], by averaging all channels of each frame. Pure and allocation-shaped for easy
/// testing with synthetic byte buffers -- no WASAPI handle needed to exercise this logic.
/// Any frame data that doesn't evenly fit the format's frame size is dropped (should never
/// happen with real WASAPI buffers, which are always a whole number of frames).
fn bytes_to_mono_f32(data: &[u8], format: CaptureFormat) -> Vec<f32> {
    let channels = format.channels.max(1) as usize;
    let bytes_per_sample = format.bytes_per_sample as usize;
    if bytes_per_sample == 0 {
        return Vec::new();
    }
    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 || data.len() < frame_bytes {
        return Vec::new();
    }

    data.chunks_exact(frame_bytes)
        .map(|frame| {
            let mut sum = 0.0f32;
            for ch in 0..channels {
                let start = ch * bytes_per_sample;
                let sample_bytes = &frame[start..start + bytes_per_sample];
                let sample = match (format.is_float, bytes_per_sample) {
                    (true, 4) => f32::from_le_bytes(sample_bytes.try_into().unwrap()),
                    (false, 2) => {
                        i16::from_le_bytes(sample_bytes.try_into().unwrap()) as f32
                            / i16::MAX as f32
                    }
                    (false, 4) => {
                        i32::from_le_bytes(sample_bytes.try_into().unwrap()) as f32
                            / i32::MAX as f32
                    }
                    // Unsupported bit depth (e.g. 8-bit or 24-bit PCM, which GetMixFormat
                    // realistically never returns for a modern render device) -- silence, not a
                    // panic or garbage reinterpretation of the wrong byte width.
                    _ => 0.0,
                };
                sum += sample;
            }
            sum / channels as f32
        })
        .collect()
}

// ==================================================================================================
// The public capture handle.
// ==================================================================================================

/// A small circular buffer's worth of capture latency -- long enough that a capture thread
/// briefly delayed by scheduling doesn't overrun it, short enough not to add noticeable lag to
/// duck detection. In 100-nanosecond units (WASAPI's native time unit).
const BUFFER_DURATION_100NS: i64 = 2_000_000; // 200ms

/// How long a capture thread sleeps between polls when there's no new audio packet waiting --
/// short enough to keep duck detection responsive, long enough not to busy-spin a whole CPU core
/// for a background feature most of whose time is spent waiting on someone to start talking.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One priority-trigger app's live process-loopback capture + speech detection, running on its
/// own dedicated OS thread for as long as this handle is alive. `WindowsMixerBackend` holds one
/// per currently-active priority app (see `windows.rs`'s `Inner::captures`), creating and
/// dropping them as the active/priority set changes.
pub struct DuckCapture {
    is_triggering: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DuckCapture {
    /// Spawns the capture thread immediately and returns without blocking -- activation is
    /// inherently asynchronous (see `activate_process_loopback_client`) with no documented upper
    /// bound on latency, so this must never run on `WindowsMixerBackend`'s poll thread. If
    /// activation or initialization fails (pid no longer exists, unsupported Windows version,
    /// anything else), the thread logs a warning and exits -- this app simply never becomes a
    /// duck trigger, the same "best-effort, degrade quietly" pattern the rest of this crate's
    /// Windows backend already uses throughout `list_sessions`.
    pub fn new(pid: u32) -> Self {
        let is_triggering = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_is_triggering = Arc::clone(&is_triggering);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            if let Err(err) = run_capture(pid, &thread_is_triggering, &thread_stop) {
                log::warn!("auto-duck capture for pid {pid} stopped: {err}");
            }
        });

        Self {
            is_triggering,
            stop,
            thread: Some(thread),
        }
    }

    pub fn is_triggering(&self) -> bool {
        self.is_triggering.load(Ordering::Relaxed)
    }
}

impl Drop for DuckCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Best-effort join: the capture thread checks `stop` at most `POLL_INTERVAL` apart, so
        // this returns quickly. If the thread already exited on its own (activation failed
        // earlier), `join` returns immediately either way.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The actual capture loop, run entirely on `DuckCapture`'s dedicated thread. Fallible so
/// `DuckCapture::new`'s spawn closure can log a single clear reason for giving up instead of the
/// thread just silently doing nothing forever.
fn run_capture(
    pid: u32,
    is_triggering: &Arc<AtomicBool>,
    stop: &Arc<AtomicBool>,
) -> Result<(), super::MixerError> {
    // Every COM-using OS thread needs its own apartment initialization -- same pattern
    // `windows.rs`'s `enumerate_session_controls` already uses. RPC_E_CHANGED_MODE/S_FALSE (some
    // form of COM already initialized on this thread) is fine, hence ignoring the result.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let audio_client = activate_process_loopback_client(pid)?;

    let (wave_format, format) = hardcoded_capture_format();
    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            BUFFER_DURATION_100NS,
            0,
            &wave_format,
            None,
        )
    }
    .map_err(|e| super::MixerError::Platform(e.to_string()))?;

    let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService() }
        .map_err(|e| super::MixerError::Platform(e.to_string()))?;
    unsafe { audio_client.Start() }.map_err(|e| super::MixerError::Platform(e.to_string()))?;

    log::info!(
        "auto-duck capture started for pid {pid}: {} ch, {} Hz, {}-bit {}",
        format.channels,
        format.sample_rate,
        format.bytes_per_sample * 8,
        if format.is_float { "float" } else { "int" }
    );

    let hysteresis = Arc::new(HysteresisCounters::default());
    // Never excluded: `WindowsMixerBackend` only ever creates a `DuckCapture` for an app that's
    // actually in the priority-trigger list (unlike macOS, which taps every active app in one
    // shared engine and uses `excluded_from_triggering` to keep non-priority ones from counting).
    let mut detector = SpeechDetector::new(false, hysteresis);
    let mut resampler = LinearResampler::new(format.sample_rate, VAD_SAMPLE_RATE_HZ);

    let mut was_triggering = false;

    while !stop.load(Ordering::Relaxed) {
        let packet_size = unsafe { capture_client.GetNextPacketSize() }
            .map_err(|e| super::MixerError::Platform(e.to_string()))?;
        if packet_size == 0 {
            std::thread::sleep(POLL_INTERVAL);
            continue;
        }

        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut num_frames: u32 = 0;
        let mut flags: u32 = 0;
        unsafe {
            capture_client
                .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
                .map_err(|e| super::MixerError::Platform(e.to_string()))?;
        }

        let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
        let mono = if is_silent || data_ptr.is_null() || num_frames == 0 {
            // Still advance with real zeros (not skipping the packet) so the VAD's frame timing
            // stays consistent with an actual gap in audio, rather than silently compressing it.
            vec![0.0f32; num_frames as usize]
        } else {
            let byte_len = num_frames as usize
                * format.channels.max(1) as usize
                * format.bytes_per_sample as usize;
            let bytes = unsafe { std::slice::from_raw_parts(data_ptr, byte_len) };
            bytes_to_mono_f32(bytes, format)
        };

        unsafe {
            capture_client
                .ReleaseBuffer(num_frames)
                .map_err(|e| super::MixerError::Platform(e.to_string()))?;
        }

        let resampled = resampler.process(&mono);
        let triggering = detector.process_frame(&resampled);
        is_triggering.store(triggering, Ordering::Relaxed);

        if triggering != was_triggering {
            log::info!(
                "auto-duck: pid {pid} trigger {}",
                if triggering { "ON" } else { "OFF" }
            );
            was_triggering = triggering;
        }
    }

    unsafe { audio_client.Stop() }.map_err(|e| super::MixerError::Platform(e.to_string()))?;
    Ok(())
}

// ==================================================================================================
// On-disk settings persistence (mirrors `macos_ducking.rs`'s, different path).
// ==================================================================================================

fn config_file_path() -> Option<std::path::PathBuf> {
    let app_data = std::env::var("APPDATA").ok()?;
    Some(
        std::path::PathBuf::from(app_data)
            .join("MiXolume")
            .join("ducking-config.json"),
    )
}

/// Loads persisted settings from disk, or the default (disabled, nothing configured) if none
/// have ever been saved, the file is unreadable, or `%APPDATA%` can't be resolved -- a missing/
/// corrupt config should never stop the app from starting, same contract as macOS's
/// `macos_ducking::load_settings`.
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

    fn format(channels: u16, bytes_per_sample: u16, is_float: bool) -> CaptureFormat {
        CaptureFormat {
            channels,
            sample_rate: 48_000,
            bytes_per_sample,
            is_float,
        }
    }

    // ---------------------------------------------------------------------------------------
    // bytes_to_mono_f32 -- pure conversion/downmix math, no WASAPI handle needed.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn converts_mono_i16_pcm() {
        let samples: [i16; 3] = [0, i16::MAX, i16::MIN];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let out = bytes_to_mono_f32(&bytes, format(1, 2, false));
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 1.0).abs() < 1e-4);
        assert!(out[2] < -0.99);
    }

    #[test]
    fn downmixes_stereo_by_averaging_channels() {
        // One frame: left = full scale positive, right = silence -> average is half scale.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&i16::MAX.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        let out = bytes_to_mono_f32(&bytes, format(2, 2, false));
        assert_eq!(out.len(), 1);
        assert!((out[0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn converts_ieee_float_passthrough() {
        let samples: [f32; 2] = [0.25, -0.5];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let out = bytes_to_mono_f32(&bytes, format(1, 4, true));
        assert_eq!(out, vec![0.25, -0.5]);
    }

    #[test]
    fn unsupported_bit_depth_yields_silence_not_garbage_or_panic() {
        // 8-bit "PCM" -- a shape this project doesn't expect GetMixFormat to ever hand back, but
        // must degrade safely rather than misinterpret bytes or panic on an out-of-range slice.
        let bytes = vec![0xFFu8, 0x7Fu8];
        let out = bytes_to_mono_f32(&bytes, format(1, 1, false));
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn empty_or_undersized_input_yields_no_samples() {
        assert!(bytes_to_mono_f32(&[], format(1, 2, false)).is_empty());
        // One byte can't form even one 2-byte mono sample.
        assert!(bytes_to_mono_f32(&[0x12], format(1, 2, false)).is_empty());
    }
}
