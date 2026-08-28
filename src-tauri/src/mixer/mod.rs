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
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub mod windows_ducking;

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
}

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
/// worth unit-testing once instead of three times.
pub fn clamp_volume(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0)
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

/// Adds (`is_priority: true`) or removes (`false`) `display_name` from `priority_triggers` --
/// the actual list-mutation logic behind every backend's `set_duck_trigger_priority`, which was
/// otherwise identical across macOS and Windows (only the settings-persistence call after it
/// differs, since that's backend-specific).
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn toggle_priority_trigger(
    priority_triggers: &mut Vec<String>,
    display_name: &str,
    is_priority: bool,
) {
    let already_present = priority_triggers.iter().any(|n| n == display_name);
    if is_priority && !already_present {
        priority_triggers.push(display_name.to_string());
    } else if !is_priority {
        priority_triggers.retain(|n| n != display_name);
    }
}

pub trait AudioMixerBackend: Send + Sync {
    /// Every app currently known to be producing (or recently produced) sound.
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError>;
    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError>;
    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError>;
    /// -1.0 (full left) to 1.0 (full right), 0.0 centered.
    fn set_balance(&self, session_id: &str, balance: f32) -> Result<(), MixerError>;

    /// Release any OS-level audio resources this backend is holding, synchronously, before the
    /// app process exits. Only macOS needs this: its backend reroutes audio through process
    /// taps + a private aggregate device, and `Drop`ping that cleanly un-mutes every tapped
    /// app's normal output path immediately instead of leaving it muted until macOS notices the
    /// (now-dead) tapping process and reclaims its Core Audio objects on its own -- which still
    /// happens, but with an audible extra gap first. Windows/Linux never mute or reroute
    /// anything; they only poke a volume/mute value on the OS's own already-persistent audio
    /// session, so there's nothing to release and the default no-op is correct for them.
    fn shutdown(&self) {}

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
}

/// One currently-running app, by name -- macOS-only internal use (matching well-known
/// communication apps for auto-duck's default-seeding, see `set_ducking_enabled` in macos.rs),
/// not exposed through [`AudioMixerBackend`] or to the frontend. The Settings "add app" picker
/// searches [`AppSession`]s instead (apps MiXolume has actually seen making sound) -- an earlier
/// version searched every running app via this type (with an icon per app), but resolving icons
/// for many apps at once turned out to be inherently slow (real per-icon AppKit decode cost) and
/// caused a multi-second/-minute Settings freeze, so it was reverted; only the name is needed
/// for matching against a known-apps list, so that's all this carries now.
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
}
