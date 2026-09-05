//! macOS-only persistence for per-app output-device routing choices. Windows needs no equivalent
//! file -- its `IAudioPolicyConfigFactory::SetPersistedDefaultAudioEndpoint` call writes the
//! per-app redirect directly into the OS's own audio policy store (see `windows_output_routing.rs`),
//! so there's nothing for MiXolume itself to persist there. macOS has no such OS-level API --
//! MiXolume does its own software mixing (see `macos.rs`'s module doc comment) -- so it owns this
//! choice the same way it already owns auto-duck's settings (`macos_ducking.rs`), which this
//! module's `config_file_path`/`load_settings`/`save_settings` shape mirrors exactly.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Persisted output-routing choices, keyed by app *display name* -- not session id, for the same
/// reason `DuckingSettings::priority_triggers` is name-keyed: a session id embeds the pid, which
/// changes every relaunch, so a routing choice keyed by it would be forgotten every time the app
/// restarts. Matches Windows' own behavior of surviving a relaunch (the OS persists that redirect
/// by executable identity, not by the transient session), so the feature behaves consistently
/// across platforms even though the underlying mechanism doesn't.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRoutingSettings {
    /// App display name -> chosen output device UID.
    pub by_app_name: HashMap<String, String>,
}

fn config_file_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support/MiXolume/output-routing-config.json"),
    )
}

/// Loads persisted settings from disk, or the default (nothing routed) if none have ever been
/// saved, the file is unreadable, or `$HOME` can't be resolved -- a missing/corrupt config should
/// never stop the app from starting.
pub fn load_settings() -> OutputRoutingSettings {
    config_file_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &OutputRoutingSettings) {
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

    #[test]
    fn round_trips_through_json_unchanged() {
        let mut settings = OutputRoutingSettings::default();
        settings
            .by_app_name
            .insert("Spotify".to_string(), "device-uid-123".to_string());

        let json = serde_json::to_string(&settings).unwrap();
        let restored: OutputRoutingSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, settings);
    }

    #[test]
    fn defaults_to_empty_when_nothing_persisted() {
        assert!(OutputRoutingSettings::default().by_app_name.is_empty());
    }
}
