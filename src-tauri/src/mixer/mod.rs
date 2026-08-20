//! Shared cross-platform audio-mixer abstraction.
//!
//! Each OS gets its own [`AudioMixerBackend`] implementation behind a `cfg(target_os = ...)`
//! module. The rest of the app (Tauri commands, the polling loop, the frontend) only ever
//! talks to the trait, never to a platform module directly.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

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
    /// 0.0 (silent) to 1.0 (full scale). Platforms that allow boosting past unity may exceed 1.0.
    pub volume: f32,
    pub muted: bool,
    /// Producing sound right now, as opposed to present-but-silent.
    pub is_active: bool,
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

pub trait AudioMixerBackend: Send + Sync {
    /// Every app currently known to be producing (or recently produced) sound.
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError>;
    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError>;
    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError>;
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
