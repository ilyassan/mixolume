//! Shared low-level WASAPI/COM primitives for Windows' Process Loopback Capture, used by both
//! auto-duck's capture (`windows_ducking.rs`, listens for speech) and volume boost's
//! capture+render (`windows_boost.rs`, actually re-emits the audio, louder). Both need the exact
//! same activation dance and the exact same capture-format workaround, so it lives here once
//! instead of twice.

use std::sync::{Arc, Condvar, Mutex};

use windows::core::{implement, Interface, Result as WinResult};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVE_FORMAT_PCM,
};

// ==================================================================================================
// COM plumbing: async activation.
// ==================================================================================================

/// Signals the waiting thread once Windows finishes (or fails) activating the process-loopback
/// virtual device. `ActivateAudioInterfaceAsync` is inherently async -- there is no synchronous
/// version of this call -- so something has to bridge it back to a plain blocking wait; a
/// `Condvar` is the simplest option since exactly one notification ever happens per activation.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    state: Arc<(Mutex<ActivationResult>, Condvar)>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        activate_operation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> WinResult<()> {
        let result = (|| -> WinResult<IAudioClient> {
            let op = activate_operation
                .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
            let mut hr = windows::core::HRESULT(0);
            let mut interface: Option<windows::core::IUnknown> = None;
            unsafe { op.GetActivateResult(&mut hr, &mut interface)? };
            hr.ok()?;
            let unknown = interface
                .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
            unknown.cast::<IAudioClient>()
        })();

        let (lock, cvar) = &*self.state;
        lock.lock().unwrap().0 = Some(result);
        cvar.notify_all();
        Ok(())
    }
}

/// Wraps `activation_params` in a `VT_BLOB` PROPVARIANT pointing directly at it -- valid only for
/// the duration of the `ActivateAudioInterfaceAsync` call this feeds, which reads the PROPVARIANT
/// synchronously before returning (it just *starts* the async activation; it doesn't retain the
/// PROPVARIANT itself afterward).
///
/// The caller MUST `std::mem::forget()` the returned value rather than let it drop normally --
/// confirmed live (via a standalone proof-of-concept, before this module existed): letting
/// `PROPVARIANT`'s `Drop` impl run `PropVariantClear` on this crashed with `STATUS_HEAP_CORRUPTION`
/// (0xC0000374). `PropVariantClear` tries to free `blob.pBlobData` for `VT_BLOB`, but that pointer
/// here is a stack address (`activation_params`), not something `CoTaskMemAlloc`'d -- there is
/// nothing real to free, and trying to corrupts the heap.
pub(super) fn blob_propvariant(
    activation_params: &AUDIOCLIENT_ACTIVATION_PARAMS,
) -> windows::core::PROPVARIANT {
    use windows::core::imp;
    unsafe {
        windows::core::PROPVARIANT::from_raw(imp::PROPVARIANT {
            Anonymous: imp::PROPVARIANT_0 {
                Anonymous: imp::PROPVARIANT_0_0 {
                    vt: windows::Win32::System::Variant::VT_BLOB.0,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: imp::PROPVARIANT_0_0_0 {
                        blob: imp::BLOB {
                            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: activation_params as *const _ as *mut u8,
                        },
                    },
                },
            },
        })
    }
}

/// Wraps the activation outcome handed from the COM completion-handler thread to whichever
/// thread is blocked waiting in [`activate_process_loopback_client`]. `IAudioClient` (like every
/// COM interface here, via `windows-core`'s `IUnknown` wrapping a bare `NonNull<c_void>`) is
/// `!Send`/`!Sync` by default -- Rust has no way to statically verify COM apartment-threading
/// rules, so the crate conservatively opts out for every interface type. That default is overly
/// strict for this specific case: every COM-using thread in this module/`windows_ducking.rs`/
/// `windows_boost.rs`/`windows.rs` initializes into the multithreaded apartment
/// (`CoInitializeEx(..., COINIT_MULTITHREADED)`), and within one MTA, interface pointers genuinely
/// are free-threaded -- safe to hand to another thread, as long as concurrent calls on the same
/// instance are externally synchronized. The `Mutex` this is stored behind already provides that
/// for the one handoff this type exists for.
pub(super) struct ActivationResult(pub(super) Option<WinResult<IAudioClient>>);
// SAFETY: see the doc comment above -- sound specifically because every thread that can touch
// this is already in the same multithreaded COM apartment.
unsafe impl Send for ActivationResult {}
unsafe impl Sync for ActivationResult {}

/// Activates process-loopback capture for `pid`, blocking the calling thread until Windows
/// finishes (or fails) the activation. Intended to be called from a dedicated capture thread
/// (see `windows_ducking::DuckCapture::new`/`windows_boost::BoostEngine::new`), never from
/// `WindowsMixerBackend`'s poll thread -- there is no documented upper bound on how long
/// activation can take.
pub(super) fn activate_process_loopback_client(
    pid: u32,
) -> Result<IAudioClient, super::MixerError> {
    let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                // Targets the exact session-owning pid `WindowsMixerBackend` already enumerates
                // (see `windows.rs`), whether or not that's the app's "main" process -- e.g. a
                // Chromium/Electron app's audio session is often owned by a renderer/utility
                // subprocess, not the top-level .exe. Per Microsoft's docs this mode captures
                // that exact pid plus any children it spawns, so it works the same either way;
                // INCLUDE (not EXCLUDE) since we want that process's audio, not everything else's.
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let propvariant = blob_propvariant(&activation_params);

    let state = Arc::new((Mutex::new(ActivationResult(None)), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        state: state.clone(),
    }
    .into();

    let activate_result = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&propvariant as *const _ as *const _),
            &handler,
        )
    };
    // See `blob_propvariant`'s doc comment -- this variant's blob points at a stack local, and
    // must never run through `PropVariantClear`.
    std::mem::forget(propvariant);
    let _operation: IActivateAudioInterfaceAsyncOperation =
        activate_result.map_err(|e| super::MixerError::Platform(e.to_string()))?;

    let (lock, cvar) = &*state;
    let mut guard = lock.lock().unwrap();
    while guard.0.is_none() {
        guard = cvar.wait(guard).unwrap();
    }
    guard
        .0
        .take()
        .unwrap()
        .map_err(|e| super::MixerError::Platform(e.to_string()))
}

// ==================================================================================================
// Format negotiation.
// ==================================================================================================

/// What a capture/render consumer needs to know to convert raw WASAPI bytes into samples: how
/// many interleaved channels, how many bytes each one takes, and whether they're IEEE float or
/// integer PCM.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CaptureFormat {
    pub(super) channels: u16,
    pub(super) sample_rate: u32,
    pub(super) bytes_per_sample: u16,
    pub(super) is_float: bool,
}

/// The format every process-loopback capture in this crate uses -- hardcoded rather than
/// queried, because process-loopback-activated `IAudioClient` objects genuinely do not implement
/// `GetMixFormat()`/`IsFormatSupported()` at all: confirmed live (an earlier version of
/// `windows_ducking.rs` called `GetMixFormat()` and got back `E_NOTIMPL`, "Non implémenté",
/// against a real Chrome session), and confirmed independently via Microsoft's own Q&A
/// (learn.microsoft.com/answers/questions/1125409) and a `Windows-classic-samples` GitHub issue
/// (microsoft/Windows-classic-samples#275): the COM class actually backing this kind of client
/// (`AudioSes!CMixerClient`) simply doesn't have those methods. Microsoft's own
/// `ApplicationLoopback` sample works around exactly this by hardcoding a known-good format
/// instead of querying one -- CD-quality (2-channel, 16-bit, 44.1kHz PCM), which every shared-mode
/// render endpoint's audio engine supports. This is that same format, for the same reason, used
/// identically by both auto-duck's capture and boost's capture.
pub(super) fn hardcoded_capture_format() -> (WAVEFORMATEX, CaptureFormat) {
    const CHANNELS: u16 = 2;
    const SAMPLE_RATE: u32 = 44_100;
    const BITS_PER_SAMPLE: u16 = 16;
    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: BITS_PER_SAMPLE,
        cbSize: 0,
    };
    let format = CaptureFormat {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bytes_per_sample: BITS_PER_SAMPLE / 8,
        is_float: false,
    };
    (wave_format, format)
}

/// A minimal streaming linear-interpolation resampler, carrying just enough state (the previous
/// chunk's last sample, and a fractional read position) across calls to resample a continuous
/// stream in independent chunks without discontinuities at chunk boundaries. Operates on a flat
/// `&[f32]` and is channel-agnostic -- callers with interleaved multi-channel audio run one
/// instance per channel against deinterleaved data (see `windows_boost.rs`).
///
/// Deliberately not a proper windowed-sinc/polyphase resampler -- auto-duck's `webrtc-vad` only
/// needs the speech band (roughly 300Hz-3.4kHz) reasonably intact to classify voice activity, and
/// boost's use is a stopgap between the hardcoded 44.1kHz capture format and whatever rate the
/// real render device actually wants, not a claim of audiophile fidelity either. Pulling in a
/// new resampling crate for either use isn't justified. Some resampling step is unavoidable for
/// process-loopback capture specifically, since its format is hardcoded (see
/// `hardcoded_capture_format`'s doc comment) rather than negotiable the way it was on macOS's tap
/// API (which lets format be chosen upfront).
pub(super) struct LinearResampler {
    /// Input samples per output sample. `< 1.0` upsamples, `> 1.0` downsamples, `1.0` is a
    /// pass-through (handled without ever entering the interpolation math, both for clarity and
    /// so a same-rate stream is bit-for-bit unmodified).
    ratio: f64,
    /// The last sample from the previous call, used as the left-hand side of the very first
    /// interpolation this call performs (so a call boundary doesn't sound like a discontinuity).
    /// `0.0` before the first call, which very slightly affects only the first fractional
    /// output sample ever produced -- inaudible and irrelevant to either consumer.
    prev: f32,
    /// How far past `prev` (in input-sample units) the next output sample should be read from,
    /// carried over from the end of the previous call.
    pos: f64,
}

impl LinearResampler {
    pub(super) fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            ratio: from_rate as f64 / to_rate as f64,
            prev: 0.0,
            pos: 0.0,
        }
    }

    pub(super) fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if (self.ratio - 1.0).abs() < f64::EPSILON {
            return input.to_vec();
        }

        // `combined[0]` is the previous call's last sample so interpolation across the chunk
        // boundary uses a real neighboring sample instead of treating the boundary as silence.
        let mut combined = Vec::with_capacity(input.len() + 1);
        combined.push(self.prev);
        combined.extend_from_slice(input);

        let mut out = Vec::new();
        let mut pos = self.pos;
        while (pos.floor() as usize) + 1 < combined.len() {
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            out.push(combined[idx] + (combined[idx + 1] - combined[idx]) * frac);
            pos += self.ratio;
        }

        self.prev = *input.last().unwrap();
        // Re-based against the *next* call's `combined[0]`, which will be this call's last
        // sample -- i.e. this call's `combined.len() - 1` (== `input.len()`) in this call's own
        // indexing.
        self.pos = pos - (combined.len() - 1) as f64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // LinearResampler -- rate conversion math, exercised with synthetic ramps/tones.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn same_rate_is_a_pure_passthrough() {
        let mut r = LinearResampler::new(48_000, 48_000);
        let input = vec![0.1, -0.2, 0.3, -0.4];
        assert_eq!(r.process(&input), input);
    }

    #[test]
    fn downsampling_produces_proportionally_fewer_samples() {
        // 48kHz -> 24kHz halves the sample count.
        let mut r = LinearResampler::new(48_000, 24_000);
        let input = vec![0.0; 1000];
        let out = r.process(&input);
        assert!(
            (out.len() as i64 - 500).abs() <= 2,
            "got {} samples",
            out.len()
        );
    }

    #[test]
    fn upsampling_produces_proportionally_more_samples() {
        // 24kHz -> 48kHz doubles the sample count.
        let mut r = LinearResampler::new(24_000, 48_000);
        let input = vec![0.0; 500];
        let out = r.process(&input);
        assert!(
            (out.len() as i64 - 1000).abs() <= 2,
            "got {} samples",
            out.len()
        );
    }

    #[test]
    fn interpolates_between_known_values() {
        // 2 input samples upsampled 4x should land roughly on the midpoint between them.
        let mut r = LinearResampler::new(1, 4);
        let out = r.process(&[0.0, 1.0]);
        assert!(out.iter().any(|&s| (s - 0.5).abs() < 0.3), "{out:?}");
    }

    #[test]
    fn stays_continuous_across_a_chunk_boundary() {
        // A steadily rising ramp fed in two separate calls should keep rising smoothly across
        // the boundary, not jump or reset -- this is what `prev`/`pos` carry-over exists for.
        let mut r = LinearResampler::new(44_100, 48_000);
        let first: Vec<f32> = (0..200).map(|i| i as f32 / 200.0).collect();
        let second: Vec<f32> = (200..400).map(|i| i as f32 / 200.0).collect();
        let mut out = r.process(&first);
        out.extend(r.process(&second));
        for pair in out.windows(2) {
            // Allow a small negative epsilon for floating-point interpolation noise, but no real
            // backward jump and no large discontinuity.
            assert!(pair[1] - pair[0] > -0.01, "{:?} -> {:?}", pair[0], pair[1]);
            assert!(
                (pair[1] - pair[0]).abs() < 0.1,
                "{:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn empty_input_produces_no_output_and_does_not_panic() {
        let mut r = LinearResampler::new(44_100, 48_000);
        assert!(r.process(&[]).is_empty());
    }
}
