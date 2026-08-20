//! Linux backend for the audio mixer abstraction.
//!
//! MVP approach (see project plan, "Linux" section): shell out to the `pactl` CLI rather than
//! linking against libpulse via FFI. This keeps the v1 implementation simple and dependency-free
//! at the cost of a process spawn per call; a native libpulse-based backend is a later iteration.
//!
//! `pactl list sink-inputs` prints one text block per sink-input, e.g.:
//!
//! ```text
//! Sink Input #123
//!         Driver: protocol-native.c
//!         Owner Module: 7
//!         Client: 34
//!         Sink: 0
//!         Sample Specification: s16le 2ch 44100Hz
//!         Channel Map: front-left,front-right
//!         Format: pcm, format.sample_format = "\"s16le\"" ...
//!         Corked: no
//!         Mute: no
//!         Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
//!                 balance 0.00
//!         Buffer Latency: 19312 usec
//!         Sink Latency: 19921 usec
//!         Resample method: soxr-mq
//!         Properties:
//!                 media.name = "Playback"
//!                 application.name = "Firefox"
//!                 application.icon_name = "firefox"
//!                 application.process.id = "5678"
//!                 application.process.binary = "firefox"
//!
//! ```
//!
//! We parse that text (see [`parse_sink_inputs`]) rather than the `--format=json` variant because
//! JSON output is only available on newer PulseAudio/pactl builds; the plain-text format has been
//! stable across PulseAudio versions for a long time and is what the plan calls for.

use std::collections::HashMap;
use std::process::Command;

use super::{clamp_volume, AppSession, AudioMixerBackend, MixerError};

pub struct LinuxMixerBackend;

impl LinuxMixerBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LinuxMixerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixerBackend for LinuxMixerBackend {
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        let output = Command::new("pactl")
            .args(["list", "sink-inputs"])
            .output()
            .map_err(|e| MixerError::Platform(format!("failed to spawn pactl: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(MixerError::Platform(format!(
                "pactl list sink-inputs failed: {stderr}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_sink_inputs(&stdout))
    }

    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError> {
        let percentage = (clamp_volume(volume) * 100.0).round() as i32;
        run_pactl(
            &[
                "set-sink-input-volume",
                session_id,
                &format!("{percentage}%"),
            ],
            session_id,
        )
    }

    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError> {
        run_pactl(
            &[
                "set-sink-input-mute",
                session_id,
                if muted { "1" } else { "0" },
            ],
            session_id,
        )
    }
}

/// Run a `pactl` subcommand that doesn't produce output we care about (the two setters), mapping
/// failure into a [`MixerError`].
///
/// We can't cheaply verify a session id exists ahead of the call without another round-trip to
/// `pactl list sink-inputs`, so we rely on pactl's own error text: PulseAudio reports an unknown
/// sink-input index as "No such entity" on stderr, which we map to `SessionNotFound`. Any other
/// failure (pactl missing, PulseAudio not running, etc.) becomes a generic `Platform` error.
fn run_pactl(args: &[&str], session_id: &str) -> Result<(), MixerError> {
    let output = Command::new("pactl")
        .args(args)
        .output()
        .map_err(|e| MixerError::Platform(format!("failed to spawn pactl: {e}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.contains("No such entity") {
        Err(MixerError::SessionNotFound(session_id.to_string()))
    } else {
        Err(MixerError::Platform(format!(
            "pactl {} failed: {stderr}",
            args.join(" ")
        )))
    }
}

/// Parse the text output of `pactl list sink-inputs` into [`AppSession`]s.
///
/// Pulled out as a standalone pure function (rather than inlined in `list_sessions`) so it's
/// unit-testable against fixture text without a real `pactl`/PulseAudio process running.
fn parse_sink_inputs(output: &str) -> Vec<AppSession> {
    let mut sessions = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in output.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Sink Input #") {
            if let Some(id) = current_id.take() {
                sessions.push(parse_block(&id, &current_lines));
            }
            current_lines.clear();
            current_id = Some(rest.trim().to_string());
        } else {
            current_lines.push(line);
        }
    }
    if let Some(id) = current_id.take() {
        sessions.push(parse_block(&id, &current_lines));
    }

    sessions
}

/// Parse the indented body of a single `Sink Input #<id>` block.
fn parse_block(id: &str, lines: &[&str]) -> AppSession {
    let mut volume = 1.0f32;
    let mut muted = false;
    let mut is_active = true;
    let mut in_properties = false;
    let mut properties: HashMap<String, String> = HashMap::new();

    for raw_line in lines {
        let trimmed = raw_line.trim();

        if trimmed.starts_with("Properties:") {
            in_properties = true;
            continue;
        }

        if in_properties {
            if let Some((key, value)) = parse_property_line(trimmed) {
                properties.insert(key, value);
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Volume:") {
            if let Some(pct) = extract_first_percentage(rest) {
                volume = clamp_volume(pct / 100.0);
            }
        } else if let Some(rest) = trimmed.strip_prefix("Mute:") {
            muted = rest.trim() == "yes";
        } else if let Some(rest) = trimmed.strip_prefix("Corked:") {
            // Corked (paused) sink-inputs are present but not currently producing sound.
            is_active = rest.trim() != "yes";
        }
    }

    let display_name = properties
        .get("application.name")
        .or_else(|| properties.get("application.process.binary"))
        .cloned()
        .unwrap_or_else(|| id.to_string());

    AppSession {
        id: id.to_string(),
        display_name,
        // PulseAudio only exposes a themed icon *name* string (application.icon_name), not PNG
        // bytes. Resolving an icon-theme name to actual image bytes is out of scope for v1.
        icon_png: None,
        volume,
        muted,
        is_active,
    }
}

/// Parse a `Properties:` section line of the form `key = "value"` into `(key, value)`.
fn parse_property_line(line: &str) -> Option<(String, String)> {
    let (key, rest) = line.split_once(" = ")?;
    let value = rest.trim();
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    Some((key.trim().to_string(), value.to_string()))
}

/// Find the first `N%` token in a string (as found in `Volume:` lines like
/// `front-left: 65536 / 100% / 0.00 dB, ...`) and return `N` as a float.
fn extract_first_percentage(s: &str) -> Option<f32> {
    let percent_idx = s.find('%')?;
    let before = &s[..percent_idx];
    let start = before
        .rfind(|c: char| !c.is_ascii_digit() && c != '.')
        .map(|i| i + 1)
        .unwrap_or(0);
    before[start..].parse::<f32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE_SINK_INPUT: &str = "Sink Input #123
\tDriver: protocol-native.c
\tOwner Module: 7
\tClient: 34
\tSink: 0
\tSample Specification: s16le 2ch 44100Hz
\tChannel Map: front-left,front-right
\tFormat: pcm
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\t        balance 0.00
\tBuffer Latency: 19312 usec
\tSink Latency: 19921 usec
\tResample method: soxr-mq
\tProperties:
\t\tmedia.name = \"Playback\"
\t\tapplication.name = \"Firefox\"
\t\tapplication.icon_name = \"firefox\"
\t\tapplication.process.id = \"5678\"
\t\tapplication.process.binary = \"firefox\"
";

    const TWO_SINK_INPUTS: &str = "Sink Input #10
\tDriver: protocol-native.c
\tCorked: no
\tMute: no
\tVolume: front-left: 45875 / 70% / -7.00 dB,   front-right: 45875 / 70% / -7.00 dB
\tProperties:
\t\tapplication.name = \"Spotify\"
\t\tapplication.process.binary = \"spotify\"

Sink Input #11
\tDriver: protocol-native.c
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tProperties:
\t\tapplication.name = \"Discord\"
\t\tapplication.process.binary = \"discord\"
";

    const MUTED_SINK_INPUT: &str = "Sink Input #42
\tDriver: protocol-native.c
\tCorked: no
\tMute: yes
\tVolume: front-left: 32768 / 50% / -18.06 dB,   front-right: 32768 / 50% / -18.06 dB
\tProperties:
\t\tapplication.name = \"Chromium\"
\t\tapplication.process.binary = \"chromium\"
";

    const CORKED_SINK_INPUT: &str = "Sink Input #7
\tDriver: protocol-native.c
\tCorked: yes
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tProperties:
\t\tapplication.name = \"VLC media player\"
\t\tapplication.process.binary = \"vlc\"
";

    const MISSING_APPLICATION_NAME: &str = "Sink Input #99
\tDriver: protocol-native.c
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tProperties:
\t\tmedia.name = \"playback\"
\t\tapplication.process.binary = \"some-daemon\"
";

    const MISSING_ALL_NAME_HINTS: &str = "Sink Input #100
\tDriver: protocol-native.c
\tCorked: no
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tProperties:
\t\tmedia.name = \"playback\"
";

    #[test]
    fn parses_single_sink_input() {
        let sessions = parse_sink_inputs(SINGLE_SINK_INPUT);
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.id, "123");
        assert_eq!(session.display_name, "Firefox");
        assert_eq!(session.volume, 1.0);
        assert!(!session.muted);
        assert!(session.is_active);
        assert!(session.icon_png.is_none());
    }

    #[test]
    fn parses_multiple_sink_inputs() {
        let sessions = parse_sink_inputs(TWO_SINK_INPUTS);
        assert_eq!(sessions.len(), 2);

        assert_eq!(sessions[0].id, "10");
        assert_eq!(sessions[0].display_name, "Spotify");
        assert!((sessions[0].volume - 0.70).abs() < 0.001);

        assert_eq!(sessions[1].id, "11");
        assert_eq!(sessions[1].display_name, "Discord");
        assert_eq!(sessions[1].volume, 1.0);
    }

    #[test]
    fn parses_muted_sink_input() {
        let sessions = parse_sink_inputs(MUTED_SINK_INPUT);
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].muted);
        assert!((sessions[0].volume - 0.50).abs() < 0.001);
    }

    #[test]
    fn parses_corked_sink_input_as_inactive() {
        let sessions = parse_sink_inputs(CORKED_SINK_INPUT);
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].is_active);
        assert_eq!(sessions[0].display_name, "VLC media player");
    }

    #[test]
    fn falls_back_to_process_binary_when_application_name_missing() {
        let sessions = parse_sink_inputs(MISSING_APPLICATION_NAME);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].display_name, "some-daemon");
    }

    #[test]
    fn falls_back_to_sink_input_id_when_no_name_hints_present() {
        let sessions = parse_sink_inputs(MISSING_ALL_NAME_HINTS);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].display_name, "100");
    }

    #[test]
    fn empty_output_yields_no_sessions() {
        assert!(parse_sink_inputs("").is_empty());
    }

    #[test]
    fn extract_first_percentage_reads_leading_volume_percentage() {
        let line = " front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB";
        assert_eq!(extract_first_percentage(line), Some(100.0));
    }

    #[test]
    fn parse_property_line_reads_quoted_key_value() {
        assert_eq!(
            parse_property_line("application.name = \"Firefox\""),
            Some(("application.name".to_string(), "Firefox".to_string()))
        );
    }
}
