//! Shared cross-platform audio-mixer abstraction.
//!
//! Each OS gets its own [`AudioMixerBackend`] implementation behind a `cfg(target_os = ...)`
//! module. The rest of the app (Tauri commands, the polling loop, the frontend) only ever
//! talks to the trait, never to a platform module directly.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_ducking;
#[cfg(target_os = "macos")]
pub mod macos_output_routing;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub mod windows_audio;
#[cfg(target_os = "windows")]
pub mod windows_ducking;
#[cfg(target_os = "windows")]
pub mod windows_output_routing;

/// Voice-activity classification/debounce logic shared by macOS's and Windows' auto-duck
/// backends -- pure `webrtc_vad` + atomics, no platform-specific code, so it's not behind a
/// `cfg(target_os = ...)` gate like the backends that use it. See its own doc comment for the
/// split rationale.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod duck_detect;

#[cfg(test)]
pub mod mock;

use serde::Serialize;
use thiserror::Error;

/// One application currently known to be producing (or recently producing) sound.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSession {
    /// Stable per-app-instance identifier. Platform-specific meaning (e.g. a Windows audio
    /// session's process id + instance, a PulseAudio sink-input index, a macOS client pid).
    pub id: String,
    pub display_name: String,
    /// PNG-encoded icon bytes, if one could be resolved.
    pub icon_png: Option<Vec<u8>>,
    /// The volume the user set -- what the slider drags from, unaffected by auto-duck. 0.0
    /// (silent) to 1.0 (full scale). Platforms that allow boosting past unity may exceed 1.0.
    pub volume: f32,
    /// What's actually coming out right now: equal to `volume` normally, or `volume` scaled down
    /// by the duck multiplier while `is_ducked` is true. This -- not `volume` -- is what the UI
    /// should actually display, so a ducked app visibly reads quieter instead of showing its
    /// full target volume while it's audibly not playing at that level.
    pub effective_volume: f32,
    pub muted: bool,
    /// Left/right stereo balance: -1.0 is full left, 0.0 is centered, 1.0 is full right.
    pub balance: f32,
    /// Producing sound right now, as opposed to present-but-silent.
    pub is_active: bool,
    /// Auto-duck currently has this app pegged as the reason everything else is quieter --
    /// implemented on macOS and Windows (see `DuckingSettings`'s doc comment), always `false` on
    /// Linux, which has no auto-duck backend yet.
    pub is_duck_trigger: bool,
    /// Auto-duck is currently lowering this app's volume because some *other* app is triggering
    /// it -- same platform support as `is_duck_trigger`.
    pub is_ducked: bool,
    /// Monotonically increasing per-session counter, bumped by the backend every time
    /// `set_volume`/`set_muted`/`set_balance` is called for this session -- lets the frontend
    /// tell a genuinely fresh read apart from one that was captured *before* its own most recent
    /// write landed, no matter how long that read's own push happens to take to actually arrive.
    ///
    /// Exists because the poll loop's `app_handle.emit()` call was confirmed live to
    /// occasionally block for 100ms+ (contending with the WebView's main thread during an active
    /// drag, which is itself busy dispatching that same drag's own IPC calls) -- long enough to
    /// blow through the frontend's fixed-duration stale-echo protection window on its own. A
    /// push whose data was read *before* a write the frontend already knows landed can still
    /// arrive *after* that window closes if delayed like this, applying stale data with nothing
    /// left to catch it. Comparing generations instead of racing a clock closes this
    /// deterministically: the frontend only ever accepts a push at least as new as what it's
    /// already written, regardless of how long the round trip took.
    pub write_generation: u64,
    /// The output device this session is currently routed to, if it's been explicitly set away
    /// from the system default -- `None` means "following whatever the system default output
    /// device is", matching how the OS itself represents "no per-app override" (see
    /// [`AudioMixerBackend::set_session_output_device`]'s doc comment). Always `None` on a
    /// backend that doesn't implement output routing yet.
    pub output_device_id: Option<String>,
}

/// One output (render) device the user could route an app's audio to -- e.g. "Speakers",
/// "Headphones (USB DAC)". `id` is backend-specific and opaque to the frontend; it's only ever
/// round-tripped back through [`AudioMixerBackend::set_session_output_device`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

/// Cross-app auto-duck settings: whether the feature runs at all, and which apps (by display
/// name -- the only identity a relaunch can't change, unlike the pid-based session id) are
/// explicitly allowed to be a duck *trigger*. Opt-in, not opt-out: an app not in this list can
/// still be *ducked* by something else, it just never *causes* ducking itself -- with an empty
/// list (the default), the feature does nothing at all until the user adds at least one app.
/// Deliberately not "everyone's a trigger by default, uncheck the ones you don't want" -- with
/// real usage, most apps someone has open are never going to be a call/voice-note source, so a
/// short curated allow-list is both the simpler mental model and the one that actually scales in
/// the Settings UI (a handful of added apps with icons, not a scrolling checklist of everything
/// that's ever played audio this session).
///
/// Implementing this needs access to a trigger app's *raw audio content* (to tell speech from
/// music/silence), not just a volume knob -- macOS gets that via Core Audio process taps
/// (`macos.rs`), Windows via WASAPI's per-process loopback capture (`windows_ducking.rs`).
/// PulseAudio (Linux's current backend, `linux.rs`) has no equivalent per-app capture mechanism
/// without a bigger architecture change (routing each app through its own null-sink/monitor), so
/// Linux still uses the trait's default no-op methods below.
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuckingSettings {
    pub enabled: bool,
    pub priority_triggers: Vec<String>,
    /// PNG icon bytes for each name in `priority_triggers`, keyed the same way -- captured the
    /// first time that app is seen actively producing sound (each backend's own `list_sessions`
    /// reuses whatever icon it already resolved for the live session, the same bytes
    /// `AppSession::icon_png` carries) and persisted here so the Settings UI can keep showing a
    /// real icon even after the app quits or across a whole run where it never made a sound.
    /// Absent for a trigger that's never been seen active since being added; pruned by
    /// `toggle_priority_trigger` when the matching name is removed.
    #[serde(default)]
    pub priority_trigger_icons: std::collections::HashMap<String, Vec<u8>>,
}

/// Upper bound on a single icon's PNG byte length before it's eligible to be cached into
/// [`DuckingSettings::priority_trigger_icons`] -- confirmed live as a real, not hypothetical,
/// safety net: an app whose bundled icon set has no representation near the intended ~128px
/// target (only, say, 16px and a 1024px master, with nothing in between -- not unusual for
/// cross-platform/Electron-style apps) falls back to encoding the full master representation,
/// producing a single icon over a megabyte. Persisted as a JSON `number[]` (one PNG byte per
/// array element, several characters each once serialized), that turned a settings file that
/// should be a few KB into one over 14MB -- which then has to be parsed synchronously before the
/// app can even open a window, confirmed live to make the whole app appear stuck/not launching.
/// A properly-sized icon at the intended target easily stays under this; skipping the rare
/// oversized outlier instead of persisting it is a far smaller cost than a corrupted-feeling
/// settings file. Deliberately generous (not tuned to the ~128px target's theoretical minimum) so
/// it only ever rejects a genuine anomaly, never a normal icon.
pub const MAX_CACHEABLE_ICON_BYTES: usize = 65_536;

/// Sanity ceiling on the ducking-settings file's own size, checked before attempting to parse it
/// at all -- a real settings file (a short trigger-name list plus each one's icon, each capped at
/// [`MAX_CACHEABLE_ICON_BYTES`]) should never come anywhere close to this even with a couple dozen
/// apps configured. Exists as a second line of defense alongside that cap, not a replacement for
/// it: the cap only prevents *future* writes from growing the file further; this protects against
/// a file that's *already* oversized for any reason (a version predating the cap, manual editing,
/// disk corruption) by refusing to parse it at all rather than blocking app startup on it -- see
/// `MAX_CACHEABLE_ICON_BYTES`'s doc comment for the confirmed, real 14MB-file/stuck-launch incident
/// this whole safety net responds to. An oversized file is treated exactly like a missing or
/// corrupt one: silently fall back to defaults rather than delay startup trying to parse it.
pub const MAX_SETTINGS_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum MixerError {
    #[error("no session found with id {0}")]
    SessionNotFound(String),
    #[error("platform audio API error: {0}")]
    Platform(String),
}

/// Clamp a requested volume into the 0.0..=1.0 range every backend agrees on for v1.
///
/// Pulled out as a free function (rather than duplicated per backend) because it's pure logic
/// worth unit-testing once instead of three times. `allow(dead_code)`: used by `windows.rs` and
/// `linux.rs`, neither of which is compiled into a macOS build, so a local macOS-only `cargo
/// check`/`clippy` sees no caller -- CI's Windows/Linux legs do.
#[allow(dead_code)]
pub fn clamp_volume(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0)
}

/// Ceiling for backends that support boosting a session past its normal 100% volume (like VLC's
/// own boosted-volume slider) -- currently macOS only, see [`AudioMixerBackend::max_volume_percent`].
/// A real Windows implementation was tried and reverted: WASAPI has no per-session volume API
/// that goes past unity, so it needs either muting-then-recapturing the same session (confirmed
/// live to blind that session's own process-loopback capture -- Windows evidently applies a
/// session's mute/volume at or before the point loopback capture taps it) or summing a second,
/// externally-rendered layer on top of the untouched original (works, but the second layer's
/// unavoidable capture/render round-trip lands slightly after the original's effectively-instant
/// output, audible as a faint echo/comb-filter coloration). The only approach real specialized
/// tools (VoiceMeeter, VB-Cable-based boosters) use for clean quality is rerouting the target app
/// to a virtual audio device via a driver -- out of scope here; even EarTrumpet, the most-used
/// per-app Windows volume mixer, doesn't attempt boost past 100% for the same reason.
/// `allow(dead_code)`: only macOS calls this outside this file's own tests, so a Windows/Linux
/// `cargo check`/`clippy` sees no non-test caller.
#[allow(dead_code)]
pub const MAX_BOOSTED_VOLUME: f32 = 1.5;

/// Same as [`clamp_volume`] but allows up to [`MAX_BOOSTED_VOLUME`] -- used only by backends that
/// actually support boosting (their `set_volume` calls this instead of `clamp_volume`), so
/// backends that don't yet are entirely unaffected by this ceiling existing at all.
#[allow(dead_code)]
pub fn clamp_boosted_volume(volume: f32) -> f32 {
    volume.clamp(0.0, MAX_BOOSTED_VOLUME)
}

/// Appends every name in `well_known_apps` that's also present in `running_names` onto
/// `priority_triggers` -- the actual matching logic behind auto-duck's first-enable default
/// seeding (macOS's and Windows' `set_ducking_enabled` each call this once). Deliberately just
/// this loop, not the whole seeding flow: each backend's own well-known-apps list and how it
/// gathers `running_names` differ for real platform reasons (macOS can enumerate every running
/// app via NSWorkspace; Windows only knows about sessions it's already seen making sound, fetched
/// under a lock this needs to stay outside of) -- sharing just the pure comparison avoids
/// duplicating it twice without forcing two genuinely different data-gathering strategies into a
/// shape that fits both.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn seed_priority_apps_from_well_known(
    priority_triggers: &mut Vec<String>,
    well_known_apps: &[&str],
    running_names: &[String],
) {
    for well_known in well_known_apps {
        if running_names.iter().any(|name| name == well_known) {
            priority_triggers.push((*well_known).to_string());
        }
    }
}

/// Adds (`is_priority: true`) or removes (`false`) `display_name` from `settings.priority_triggers`
/// -- the actual list-mutation logic behind every backend's `set_duck_trigger_priority`, which was
/// otherwise identical across macOS and Windows (only the settings-persistence call after it
/// differs, since that's backend-specific). Removing also drops `display_name`'s entry from
/// `settings.priority_trigger_icons`, if any -- no reason to keep a cached icon around for a name
/// no longer in the list.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn toggle_priority_trigger(
    settings: &mut DuckingSettings,
    display_name: &str,
    is_priority: bool,
) {
    let already_present = settings.priority_triggers.iter().any(|n| n == display_name);
    if is_priority && !already_present {
        settings.priority_triggers.push(display_name.to_string());
    } else if !is_priority {
        settings.priority_triggers.retain(|n| n != display_name);
        settings.priority_trigger_icons.remove(display_name);
    }
}

pub trait AudioMixerBackend: Send + Sync {
    /// Every app currently known to be producing (or recently produced) sound.
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError>;
    /// Returns the session's new `write_generation` (see [`AppSession::write_generation`]) on
    /// success, so the frontend can record exactly which write it just made without waiting for
    /// a subsequent `list_sessions` push to tell it.
    fn set_volume(&self, session_id: &str, volume: f32) -> Result<u64, MixerError>;
    fn set_muted(&self, session_id: &str, muted: bool) -> Result<u64, MixerError>;
    /// -1.0 (full left) to 1.0 (full right), 0.0 centered.
    fn set_balance(&self, session_id: &str, balance: f32) -> Result<u64, MixerError>;

    /// Release any OS-level audio resources this backend is holding, synchronously, before the
    /// app process exits. Only macOS needs this: its backend reroutes audio through process
    /// taps + a private aggregate device, and `Drop`ping that cleanly un-mutes every tapped
    /// app's normal output path immediately instead of leaving it muted until macOS notices the
    /// (now-dead) tapping process and reclaims its Core Audio objects on its own -- which still
    /// happens, but with an audible extra gap first. Windows/Linux never mute or reroute
    /// anything; they only poke a volume/mute value on the OS's own already-persistent audio
    /// session, so there's nothing to release and the default no-op is correct for them.
    fn shutdown(&self) {}

    /// The highest volume percent this backend allows a session to be set to -- 100 for every
    /// backend that doesn't override it. A backend that supports boosting past unity (see
    /// [`MAX_BOOSTED_VOLUME`]) overrides this so the frontend knows to let its slider go further.
    fn max_volume_percent(&self) -> u32 {
        100
    }

    /// Current auto-duck settings. Default: disabled, no priority apps -- correct as-is for
    /// every backend that doesn't override it.
    fn get_ducking_settings(&self) -> DuckingSettings {
        DuckingSettings::default()
    }
    fn set_ducking_enabled(&self, _enabled: bool) -> Result<(), MixerError> {
        Ok(())
    }
    /// Adds (`is_priority: true`) or removes (`false`) an app from the duck-trigger allow-list.
    fn set_duck_trigger_priority(
        &self,
        _display_name: &str,
        _is_priority: bool,
    ) -> Result<(), MixerError> {
        Ok(())
    }

    /// Whether this backend can route an individual app's audio to a specific output device --
    /// false for every backend that doesn't override it, so the frontend can hide the device
    /// picker entirely rather than show a control that would silently no-op, the same
    /// capability-flag pattern `ducking_supported` already uses.
    fn output_routing_supported(&self) -> bool {
        false
    }
    /// Every currently available output (render) device. Empty for a backend that doesn't
    /// implement output routing -- pairs with `output_routing_supported`, which the frontend
    /// checks first, so an empty list here is never itself ambiguous with "supported but none
    /// found" in practice.
    fn list_output_devices(&self) -> Result<Vec<OutputDevice>, MixerError> {
        Ok(Vec::new())
    }
    /// Routes `session_id`'s audio to `device_id`, or back to the system default when `None`.
    fn set_session_output_device(
        &self,
        _session_id: &str,
        _device_id: Option<&str>,
    ) -> Result<(), MixerError> {
        Ok(())
    }
}

/// One currently-running app, by name -- macOS-only internal use (matching well-known
/// communication apps for auto-duck's default-seeding, see `set_ducking_enabled` in macos.rs),
/// not exposed through [`AudioMixerBackend`] or to the frontend. The Settings "add app" picker
/// searches [`AppSession`]s instead (apps MiXolume has actually seen making sound) -- an earlier
/// version searched every running app via this type (with an icon per app), but resolving icons
/// for many apps at once turned out to be inherently slow (real per-icon AppKit decode cost) and
/// caused a multi-second/-minute Settings freeze, so it was reverted; only the name is needed
/// for matching against a known-apps list, so that's all this carries now.
/// Only constructed by macOS's `list_running_applications` -- genuinely dead code on any other
/// target, not something to find a Windows/Linux use for.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RunningAppInfo {
    pub name: String,
}

/// Construct the real backend for whichever OS this binary is compiled for.
pub fn new_platform_backend() -> Box<dyn AudioMixerBackend> {
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsMixerBackend::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxMixerBackend::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacosMixerBackend::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_volume_clamps_below_zero() {
        assert_eq!(clamp_volume(-0.5), 0.0);
    }

    #[test]
    fn clamp_volume_clamps_above_one() {
        assert_eq!(clamp_volume(1.5), 1.0);
    }

    #[test]
    fn clamp_volume_passes_through_valid_range() {
        assert_eq!(clamp_volume(0.42), 0.42);
    }

    #[test]
    fn clamp_boosted_volume_clamps_below_zero() {
        assert_eq!(clamp_boosted_volume(-0.5), 0.0);
    }

    #[test]
    fn clamp_boosted_volume_allows_past_unity() {
        assert_eq!(clamp_boosted_volume(1.5), 1.5);
    }

    #[test]
    fn clamp_boosted_volume_clamps_above_ceiling() {
        assert_eq!(clamp_boosted_volume(5.0), MAX_BOOSTED_VOLUME);
    }
}
