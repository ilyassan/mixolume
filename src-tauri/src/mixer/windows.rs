//! Windows backend: WASAPI audio sessions via the documented Core Audio COM interfaces.
//!
//! `IMMDeviceEnumerator` -> default render endpoint -> `IAudioSessionManager2` ->
//! `IAudioSessionEnumerator` -> one `IAudioSessionControl2` per app producing sound.
//! `IAudioSessionControl2` gives us the owning process id; `ISimpleAudioVolume` (obtained by
//! casting the same session control) gives us get/set volume + mute. No elevated privileges,
//! no driver — see PLAN.md section 2.
//!
//! Auto-duck (see `windows_ducking.rs`) adds one wrinkle unique to this backend: unlike macOS
//! (which never touches a real OS volume control -- it builds its own gain pipeline from
//! scratch), ducking here has to temporarily overwrite the *actual* `ISimpleAudioVolume` value to
//! make a session quieter, then restore it. So `Inner::target_volume` tracks what the user
//! actually set each session to, independent of whatever a duck has transiently written --
//! `list_sessions`'s `volume` field always reports that tracked target (never a possibly-ducked
//! live read), matching `AppSession::volume`'s "unaffected by auto-duck" contract.
//!
//! No volume boost past 100% here (unlike macOS) -- deliberately reverted after a real attempt.
//! See [`crate::mixer::MAX_BOOSTED_VOLUME`]'s doc comment for the full rationale: WASAPI has no
//! per-session volume API past unity, and every way found to work around that either blinds this
//! same session's own process-loopback capture (confirmed live) or has an audible echo/comb-filter
//! ceiling with no clean fix short of a virtual-audio-device driver, which is out of scope.
//!
//! Per-app output-device routing (`windows_output_routing.rs`) went through two real, confirmed
//! bugs before landing on the implementation here: (1) writing/reading only the `eConsole` role
//! left the override incomplete, since different apps resolve their default endpoint via
//! different roles -- fixed by covering both `eConsole` and `eMultimedia`, matching EarTrumpet's
//! own working implementation exactly (confirmed by reading its actual MIT-licensed source, not
//! just its docs); (2) a call from a thread that never joined the WinRT multithreaded apartment
//! (Tauri's command-dispatch pool, or the poll loop after resuming on a different tokio worker)
//! silently failed -- fixed by joining defensively on every call, not just once at activation.
//! See `windows_output_routing.rs`'s own module doc comment for the full account, including a
//! real, live-observed cost of getting this undocumented interface wrong: a bad write once left
//! a real machine unable to auto-switch to headphones on plug-in even after Mixolume was killed,
//! recoverable only via `IAudioPolicyConfigFactory::ClearAllPersistedApplicationDefaultEndpoints`
//! (not a reinstall or reboot). That's why every call site here is deliberately conservative
//! about apartment-joining and role coverage rather than assuming either is a one-time concern.
//!
//! Some apps (confirmed live with Zoom) run as more than one OS process that each open their
//! own independent WASAPI session under the exact same display name -- Windows' own native
//! Volume Mixer shows this too, it's not specific to this app. `group_sessions_by_display_name`
//! merges those into one row per `list_sessions` call, and `resolve_member_pids` lets
//! `set_volume`/`set_muted`/`set_balance`/`set_session_output_device` fan a single UI action out
//! to every process that's really "the same app", instead of controlling only one of them.
//! Browser tabs were considered for the same treatment and ruled out: a browser opens exactly
//! one WASAPI session per *process*, not per tab (confirmed via Chromium's own audio
//! architecture) -- there's nothing at this layer to even distinguish one tab's audio from
//! another's, so per-tab control would need to live inside the browser itself, not here.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateActive, AudioSessionStateExpired,
    AudioSessionStateInactive, IAudioSessionControl2, IAudioSessionManager2, IChannelAudioVolume,
    IMMDeviceCollection, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

use super::duck_detect::DUCK_GAIN_MULTIPLIER;
use super::windows_ducking::{self, DuckCapture};
use super::windows_output_routing;
use super::{
    clamp_volume, AppSession, AudioMixerBackend, DuckingSettings, MixerError, OutputDevice,
};

struct Inner {
    /// Cross-app auto-duck settings, loaded from disk once at startup (see
    /// `windows_ducking::load_settings`) and updated in place as the user changes them from
    /// Settings.
    ducking_settings: DuckingSettings,
    /// One live process-loopback capture per currently-active priority-trigger app, keyed by
    /// session id. Reconciled against `ducking_settings.priority_triggers` and each session's
    /// `is_active` on every `list_sessions` call -- dropping an entry here stops its capture
    /// thread (see `DuckCapture`'s `Drop` impl).
    captures: HashMap<String, DuckCapture>,
    /// What the user actually set each session's volume to, independent of whatever a duck has
    /// transiently written to the real WASAPI volume -- see this module's doc comment. Seeded
    /// lazily from a live read the first time a session is ever seen, then only ever updated by
    /// `set_volume`.
    target_volume: HashMap<String, f32>,
    /// Whether the *last* value this backend wrote for each session was the ducked (multiplied)
    /// one -- compared against the freshly-computed value on every `list_sessions` tick so a
    /// `SetMasterVolume`/`BoostEngine::set_gain` write only happens on an actual duck-state
    /// transition, not every single ~150ms poll regardless of whether anything changed (which
    /// would mean constantly writing a volume even for sessions nothing is currently ducking).
    applied_ducked: HashMap<String, bool>,
    /// Per-session `write_generation` (see `AppSession::write_generation`'s doc comment), bumped
    /// by every `set_volume`/`set_muted`/`set_balance` call.
    write_generations: HashMap<String, u64>,
    /// Cached `IAudioPolicyConfigFactory` instance (see `windows_output_routing.rs`), lazily
    /// activated on first use and reused for the rest of the backend's lifetime, rather than
    /// re-running a real WinRT activation (`RoGetActivationFactory`) once per active session on
    /// every single ~150ms poll tick for as long as the app runs -- a real, ongoing cost with an
    /// easy fix, not just a one-time setup step to shave off.
    output_policy_factory: Option<windows_output_routing::PolicyConfigFactory>,
    /// The most recent *raw*, undebounced `get_session_output_device` read per session -- see
    /// `output_device_confirmed`'s doc comment for what this is debounced against.
    output_device_raw: HashMap<String, Option<String>>,
    /// The debounced, currently-trusted `output_device_id` per session -- only overwritten once
    /// a raw read comes back identical to the previous tick's raw read, i.e. after two
    /// consecutive polls agree. Exists because `IAudioPolicyConfigFactory::
    /// GetPersistedDefaultAudioEndpoint` was confirmed live to occasionally report a spurious
    /// non-empty device for a session that was never given an explicit per-app override, for
    /// roughly one poll tick right around a device being physically plugged in, before settling
    /// back to empty on its own -- surfacing that single-tick blip directly showed up in the UI
    /// as a device option appearing selected for a few seconds and then reverting, for a session
    /// the user never touched. Requiring two consecutive identical reads before trusting a
    /// change costs one extra ~150ms tick of latency on a genuine change, which is imperceptible.
    output_device_confirmed: HashMap<String, Option<String>>,
    /// Maps a *group* session id (see this module's doc comment and
    /// `group_sessions_by_display_name`) to every pid that's currently a member -- rebuilt from
    /// scratch on every `list_sessions` call, never accumulated. Read by `resolve_member_pids`
    /// so `set_volume`/`set_muted`/`set_balance`/`set_session_output_device` can fan a single
    /// write out to every pid the merged row actually represents. A display name only one pid
    /// currently has is never inserted here at all, so a session id's absence unambiguously
    /// means "not a group, parse it as `win-{pid}` instead".
    session_groups: HashMap<String, Vec<u32>>,
}

/// Windows backend: per-app volume via WASAPI audio sessions, auto-duck via WASAPI process
/// loopback capture. See this module's doc comment for the ducking-specific state-tracking
/// rationale.
pub struct WindowsMixerBackend {
    inner: Mutex<Inner>,
}

impl WindowsMixerBackend {
    pub fn new() -> Self {
        let ducking_settings = windows_ducking::load_settings();
        Self {
            inner: Mutex::new(Inner {
                ducking_settings,
                captures: HashMap::new(),
                target_volume: HashMap::new(),
                applied_ducked: HashMap::new(),
                write_generations: HashMap::new(),
                output_policy_factory: None,
                output_device_raw: HashMap::new(),
                output_device_confirmed: HashMap::new(),
                session_groups: HashMap::new(),
            }),
        }
    }
}

impl Default for WindowsMixerBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns `Inner`'s cached output-routing factory, activating and caching one first if this is
/// the first call -- see `Inner::output_policy_factory`'s doc comment for why this is cached
/// rather than activated fresh every call. `None` if activation itself ever fails (best-effort,
/// matching this backend's existing degrade-quietly stance elsewhere); a later call retries
/// rather than permanently remembering the failure, since nothing here rules out a transient
/// cause.
fn output_policy_factory(
    inner: &mut Inner,
) -> Option<&windows_output_routing::PolicyConfigFactory> {
    if inner.output_policy_factory.is_none() {
        inner.output_policy_factory = windows_output_routing::PolicyConfigFactory::activate().ok();
    }
    inner.output_policy_factory.as_ref()
}

/// Debounces a freshly-read `output_device_id` against a transient single-tick blip -- see
/// `Inner::output_device_confirmed`'s doc comment for why this exists. `raw` is this tick's
/// undebounced read; the first-ever read for a given `session_id` is trusted immediately
/// (there's nothing to debounce against yet), matching how `target_volume` seeds itself lazily
/// from the first live read elsewhere in this file.
fn debounce_output_device(
    inner: &mut Inner,
    session_id: &str,
    raw: Option<String>,
) -> Option<String> {
    let previous_raw = inner
        .output_device_raw
        .insert(session_id.to_string(), raw.clone());
    let confirm = match &previous_raw {
        None => true,
        Some(previous) => previous == &raw,
    };
    if confirm {
        inner
            .output_device_confirmed
            .insert(session_id.to_string(), raw);
    }
    inner
        .output_device_confirmed
        .get(session_id)
        .cloned()
        .flatten()
}

/// Bumps and returns the new `write_generation` for `session_id` -- called by every setter,
/// under whatever lock on `Inner` that setter already holds.
fn bump_generation(inner: &mut Inner, session_id: &str) -> u64 {
    let entry = inner
        .write_generations
        .entry(session_id.to_string())
        .or_insert(0);
    *entry += 1;
    *entry
}

/// Recognized by exact display-name match against apps MiXolume has already seen producing
/// sound (see `set_ducking_enabled`'s use of `list_sessions` instead of a full running-process
/// scan -- Windows has no cheap equivalent of macOS's `NSWorkspace` enumeration, and this crate's
/// `AppSession`s already only ever surface apps that have actually made sound). Same list as
/// macOS's `WELL_KNOWN_COMMUNICATION_APPS` -- entries that only exist as macOS app names
/// (FaceTime, Messages) simply never match here, which is harmless.
const WELL_KNOWN_COMMUNICATION_APPS: &[&str] = &[
    "WhatsApp",
    "Zoom",
    "Discord",
    "FaceTime",
    "Microsoft Teams",
    "Slack",
    "Messages",
    "Skype",
    "Telegram",
    "Signal",
];

/// Active(2) > Inactive(1) > Expired/unknown(0) -- the fallback tie-break in
/// [`resolve_session_control`] when a pid has multiple candidates and none matches its actual
/// routed device.
fn session_state_rank(state: windows::Win32::Media::Audio::AudioSessionState) -> u8 {
    if state == AudioSessionStateActive {
        2
    } else if state == AudioSessionStateInactive {
        1
    } else {
        0
    }
}

/// The system default render device's full PnP device-interface path -- same format
/// `list_output_devices`/persisted per-app overrides use (see `render_device_interface_path`).
/// The implicit "this pid's audio should be here" answer for a pid that has no explicit
/// override at all, used by [`resolve_session_control`].
fn system_default_device_path() -> Option<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let device_enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = device_enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()?;
        let mmdevice_id = device.GetId().ok()?.to_string().ok()?;
        Some(windows_output_routing::render_device_interface_path(
            &mmdevice_id,
        ))
    }
}

/// Every non-expired, non-system audio session control currently registered with *any* active
/// render device's session manager, grouped by owning pid (`0` excluded) -- not deduped here.
///
/// Checking only the default device was correct until per-app output routing existed
/// (`windows_output_routing.rs`): a session that's been explicitly routed to some other device
/// (e.g. headphones) stops being enumerable through the *default* device's session manager at
/// all once its audio actually starts rendering elsewhere. Each active device has its own
/// independent session manager, so correctness requires checking all of them.
///
/// A pid can genuinely have more than one live entry across devices at once -- confirmed live: a
/// stale entry left behind on the device a session used to be routed to (still reporting `Active`
/// for a while, not just `Inactive`) alongside the real one on its current device. Deduping that
/// down to one control *here*, before a caller even knows which device a given pid should
/// actually be on, was confirmed to pick the wrong one often enough to matter -- see
/// [`resolve_session_control`], which every caller uses instead of trusting a single blind pick.
fn enumerate_session_controls_by_pid(
) -> windows::core::Result<HashMap<u32, Vec<(String, IAudioSessionControl2)>>> {
    unsafe {
        // Ignore the return value: RPC_E_CHANGED_MODE / S_FALSE both mean "some form of COM is
        // already initialized on this thread", which is fine for our purposes.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let device_enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices: IMMDeviceCollection =
            device_enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let device_count = devices.GetCount()?;

        let mut by_pid: HashMap<u32, Vec<(String, IAudioSessionControl2)>> = HashMap::new();
        for device_index in 0..device_count {
            // Best-effort per device: a device that fails to activate a session manager (e.g. a
            // capture-only endpoint that slipped through, or a transient device-state change)
            // just contributes no sessions, rather than failing the whole enumeration.
            let Ok(device) = devices.Item(device_index) else {
                continue;
            };
            let Some(mmdevice_id) = device.GetId().ok().and_then(|id| id.to_string().ok()) else {
                continue;
            };
            let device_path = windows_output_routing::render_device_interface_path(&mmdevice_id);
            let Ok(session_manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
            else {
                continue;
            };
            let Ok(session_enum) = session_manager.GetSessionEnumerator() else {
                continue;
            };
            let Ok(count) = session_enum.GetCount() else {
                continue;
            };

            for i in 0..count {
                let Ok(control) = session_enum.GetSession(i) else {
                    continue;
                };
                let Ok(control2): windows::core::Result<IAudioSessionControl2> = control.cast()
                else {
                    continue;
                };
                let Ok(pid) = control2.GetProcessId() else {
                    continue;
                };
                // pid 0 sessions (no owning process) are filtered out downstream in
                // `list_sessions` anyway -- skip keeping them at all rather than needing a
                // separate non-deduped bucket for something never surfaced.
                if pid == 0 {
                    continue;
                }
                by_pid
                    .entry(pid)
                    .or_default()
                    .push((device_path.clone(), control2));
            }
        }
        Ok(by_pid)
    }
}

/// Picks the one true session control for `pid` out of possibly-several `candidates` spread
/// across devices (see `enumerate_session_controls_by_pid`'s doc comment for why more than one
/// can genuinely coexist). Prefers whichever device `pid` is actually persisted-routed to (via
/// `output_policy_factory`, or the system default if it has no override) -- the authoritative
/// answer to "where is this app's audio really going" -- falling back to the highest-ranked
/// ([`session_state_rank`]) candidate only if none matches that device (e.g. its stream hasn't
/// been (re)created there yet). Skips the routing lookup entirely when there's only one
/// candidate, which is the overwhelming majority of calls -- no reason to pay for an extra WinRT
/// round trip when there's no ambiguity to resolve.
///
/// This is the real fix for a bug that was confirmed live: muting, then unmuting, a session that
/// had just been rerouted to a different device could leave it stuck silent -- two genuinely
/// `Active` candidates for the same pid (the real one on its current device, and a stale one
/// left behind on the device it used to be on) used to tie-break on enumeration order alone,
/// which isn't guaranteed to pick the same one on every call. The mute call could hit one
/// candidate while the unmute call, enumerating fresh, hit the *other* -- leaving the genuinely
/// playing one muted forever while every subsequent volume/mute change landed on an orphaned
/// session object nobody could hear.
fn resolve_session_control(
    inner: &mut Inner,
    pid: u32,
    candidates: &[(String, IAudioSessionControl2)],
) -> Option<IAudioSessionControl2> {
    if candidates.len() > 1 {
        let preferred = output_policy_factory(inner)
            .and_then(|factory| windows_output_routing::get_session_output_device(factory, pid))
            .or_else(system_default_device_path);
        if let Some(preferred) = preferred {
            if let Some((_, control)) = candidates
                .iter()
                .find(|(device_id, _)| *device_id == preferred)
            {
                return Some(control.clone());
            }
        }
    }
    candidates
        .iter()
        .max_by_key(|(_, control)| unsafe {
            session_state_rank(control.GetState().unwrap_or(AudioSessionStateExpired))
        })
        .map(|(_, control)| control.clone())
}

fn session_id_for(pid: u32) -> String {
    format!("win-{pid}")
}

fn pid_from_session_id(id: &str) -> Option<u32> {
    id.strip_prefix("win-").and_then(|s| s.parse::<u32>().ok())
}

/// The pid(s) `session_id` actually refers to -- more than one if it's a *group* id (see this
/// module's doc comment and `group_sessions_by_display_name`), otherwise exactly the single pid
/// `win-{pid}` encodes. Every setter (`set_volume`/`set_muted`/`set_balance`/
/// `set_session_output_device`) starts here instead of parsing `session_id` directly, so a
/// write against a merged row's id fans out to every process it actually represents.
fn resolve_member_pids(inner: &Inner, session_id: &str) -> Result<Vec<u32>, MixerError> {
    if let Some(pids) = inner.session_groups.get(session_id) {
        return Ok(pids.clone());
    }
    pid_from_session_id(session_id)
        .map(|pid| vec![pid])
        .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))
}

fn find_session_control(
    inner: &mut Inner,
    session_id: &str,
) -> Result<IAudioSessionControl2, MixerError> {
    let target_pid = pid_from_session_id(session_id)
        .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
    let controls_by_pid =
        enumerate_session_controls_by_pid().map_err(|e| MixerError::Platform(e.to_string()))?;
    let candidates = controls_by_pid
        .get(&target_pid)
        .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
    resolve_session_control(inner, target_pid, candidates)
        .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))
}

/// Merges every `AppSession` sharing a `display_name` into one representative row, and records
/// each merged group's current member pids in `inner.session_groups` (cleared and rebuilt here
/// every call) -- see this module's doc comment for why this exists (confirmed live with Zoom)
/// and `resolve_member_pids` for how a setter fans a write back out to every member.
///
/// A display name only one pid currently has passes through completely untouched -- the
/// overwhelmingly common case, and it costs nothing extra: no group is recorded for it at all.
///
/// For a real group, the merged row's own fields come from whichever member has the *lowest*
/// pid (an arbitrary but stable-ish choice -- it only changes if that specific process exits,
/// not on every tick just because `HashMap` iteration order isn't fixed), except:
/// - `is_active`/`is_duck_trigger`/`is_ducked`: true if *any* member is, so the merged row
///   reflects the group as a whole rather than hiding a still-active member behind a quiet one.
/// - `icon_png`: the first member that actually has one, in case the anchor's own resolution
///   happened to fail for it specifically (rare, but no reason to show no icon when another
///   member's succeeded).
/// - `write_generation`: read from `inner.write_generations` under the *group* id, since that's
///   the only id `set_volume`/`set_muted`/`set_balance` ever bump a generation for once a
///   session is part of a group (see `resolve_member_pids`'s callers) -- each member's own
///   per-pid generation entry is irrelevant once it's absorbed into a group, since nothing
///   reads it directly anymore.
fn group_sessions_by_display_name(inner: &mut Inner, sessions: Vec<AppSession>) -> Vec<AppSession> {
    let mut by_name: HashMap<String, Vec<AppSession>> = HashMap::new();
    for session in sessions {
        by_name
            .entry(session.display_name.clone())
            .or_default()
            .push(session);
    }

    inner.session_groups.clear();
    let mut merged = Vec::with_capacity(by_name.len());
    for (_, mut members) in by_name {
        if members.len() == 1 {
            merged.push(members.pop().expect("just checked len() == 1"));
            continue;
        }
        members.sort_by_key(|s| pid_from_session_id(&s.id).unwrap_or(u32::MAX));
        let member_pids: Vec<u32> = members
            .iter()
            .filter_map(|s| pid_from_session_id(&s.id))
            .collect();
        let group_id = format!("win-group-{}", member_pids[0]);
        inner
            .session_groups
            .insert(group_id.clone(), member_pids.clone());

        let anchor = members.remove(0);
        let is_active = anchor.is_active || members.iter().any(|s| s.is_active);
        let is_duck_trigger = anchor.is_duck_trigger || members.iter().any(|s| s.is_duck_trigger);
        let is_ducked = anchor.is_ducked || members.iter().any(|s| s.is_ducked);
        let icon_png = anchor
            .icon_png
            .clone()
            .or_else(|| members.iter().find_map(|s| s.icon_png.clone()));
        let write_generation = inner.write_generations.get(&group_id).copied().unwrap_or(0);

        merged.push(AppSession {
            id: group_id,
            display_name: anchor.display_name.clone(),
            icon_png,
            volume: anchor.volume,
            effective_volume: anchor.effective_volume,
            muted: anchor.muted,
            balance: anchor.balance,
            is_active,
            is_duck_trigger,
            is_ducked,
            write_generation,
            output_device_id: anchor.output_device_id.clone(),
        });
    }
    merged
}

/// Resolve a friendly display name and, best-effort, a PNG-encoded icon for the given pid.
/// Falls back to `PID <n>` / no icon if the process can't be opened (e.g. protected processes) —
/// this is expected for some system processes and shouldn't fail the whole session list.
fn resolve_process_info(pid: u32) -> (String, Option<Vec<u8>>) {
    let exe_path = match exe_path_for_pid(pid) {
        Some(p) => p,
        None => return (format!("PID {pid}"), None),
    };

    let display_name = std::path::Path::new(&exe_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("PID {pid}"));

    let icon_png = extract_icon_png(&exe_path);
    (display_name, icon_png)
}

fn exe_path_for_pid(pid: u32) -> Option<String> {
    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 512];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        result.ok()?;
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

fn extract_icon_png(exe_path: &str) -> Option<Vec<u8>> {
    unsafe {
        let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = SHFILEINFOW::default();
        let flags = SHGFI_ICON | SHGFI_LARGEICON;
        let cb_size = std::mem::size_of::<SHFILEINFOW>() as u32;
        let result = SHGetFileInfoW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            cb_size,
            flags,
        );
        if result == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let png = hicon_to_png(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        png
    }
}

/// Converts an HICON's color plane into a PNG via GDI `GetDIBits` + the `image` crate's encoder.
/// Best-effort: any failure along the way just means no icon for that app, not a hard error.
fn hicon_to_png(hicon: HICON) -> Option<Vec<u8>> {
    unsafe {
        let mut icon_info = ICONINFO::default();
        GetIconInfo(hicon, &mut icon_info).ok()?;

        let mut bmp = BITMAP::default();
        let bmp_size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(
            icon_info.hbmColor,
            bmp_size,
            Some(&mut bmp as *mut _ as *mut _),
        ) == 0
        {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DeleteObject(icon_info.hbmMask);
            return None;
        }

        let width = bmp.bmWidth;
        let height = bmp.bmHeight;
        if width <= 0 || height <= 0 {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DeleteObject(icon_info.hbmMask);
            return None;
        }

        let hdc = GetDC(None);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // negative = top-down DIB, matches our row order below
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut buffer = vec![0u8; (width as usize) * (height as usize) * 4];
        let scanlines = GetDIBits(
            hdc,
            icon_info.hbmColor,
            0,
            height as u32,
            Some(buffer.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        let _ = DeleteObject(icon_info.hbmColor);
        let _ = DeleteObject(icon_info.hbmMask);

        if scanlines == 0 {
            return None;
        }

        // GDI gives us BGRA; the image crate / PNG wants RGBA.
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        let img = image::RgbaImage::from_raw(width as u32, height as u32, buffer)?;
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .ok()?;
        Some(out)
    }
}

/// One session's raw, freshly-enumerated WASAPI state -- gathered in `list_sessions`'s first
/// pass (which needs COM interfaces live) before it takes `Inner`'s lock for the ducking
/// reconciliation pass, keeping the two concerns (WASAPI enumeration vs. this backend's own
/// tracked state) from being interleaved in one long `unsafe` block.
struct RawSession {
    id: String,
    simple_volume: ISimpleAudioVolume,
    live_volume: f32,
    muted: bool,
    balance: f32,
    is_active: bool,
    display_name: String,
    icon_png: Option<Vec<u8>>,
}

impl AudioMixerBackend for WindowsMixerBackend {
    // No `max_volume_percent` override -- stays at the trait's default 100. See
    // `crate::mixer::MAX_BOOSTED_VOLUME`'s doc comment for why a real boost implementation was
    // tried and reverted here.

    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        let controls_by_pid =
            enumerate_session_controls_by_pid().map_err(|e| MixerError::Platform(e.to_string()))?;

        // Auto-duck's own capture threads (`windows_ducking.rs`) call
        // `ActivateAudioInterfaceAsync`/`IAudioClient::Initialize` from *this* process to capture
        // some other app's audio -- but Windows' audio session manager attributes the resulting
        // session to the calling process (us), not the app actually being captured. Confirmed
        // live: enabling auto-duck made "MiXolume" itself start appearing as a fake session in
        // this exact enumeration, immediately eligible to be shown in the UI, picked as a
        // priority-trigger app, or ducked -- any of which risks this app trying to capture/duck
        // itself. Filtering our own pid out at the source, unconditionally, is simpler and more
        // robust than trying to special-case it at every later use site.
        let own_pid = std::process::id();

        // Locked here, earlier than the rest of this function strictly needs on its own, because
        // `resolve_session_control` (used per pid below) needs the cached output-routing factory
        // to correctly pick between more than one live candidate for the same pid -- see its own
        // doc comment.
        let mut inner = self.inner.lock().unwrap();

        let mut raw = Vec::new();
        for (pid, candidates) in &controls_by_pid {
            if *pid == 0 || *pid == own_pid {
                continue;
            }
            let Some(control) = resolve_session_control(&mut inner, *pid, candidates) else {
                continue;
            };
            unsafe {
                // `IsSystemSoundsSession` returns a raw HRESULT: S_OK (0) means "yes", S_FALSE
                // (1) means "no" -- both are non-negative, so `.is_ok()` (which only checks
                // "not a failure code") is true for EVERY session and would skip all of them.
                // We must compare against S_OK specifically.
                if control.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    continue;
                }

                let state = control.GetState().unwrap_or(AudioSessionStateExpired);
                if state == AudioSessionStateExpired {
                    continue;
                }

                let simple_volume: ISimpleAudioVolume = match control.cast() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let live_volume = simple_volume.GetMasterVolume().unwrap_or(0.0);
                let muted = simple_volume
                    .GetMute()
                    .map(|b| b.as_bool())
                    .unwrap_or(false);
                let balance = read_balance(&control).unwrap_or(0.0);

                let (display_name, icon_png) = resolve_process_info(*pid);

                raw.push(RawSession {
                    id: session_id_for(*pid),
                    simple_volume,
                    live_volume,
                    muted,
                    balance,
                    is_active: state == AudioSessionStateActive,
                    display_name,
                    icon_png,
                });
            }
        }

        // Seed each newly-seen session's tracked target volume from its current live value --
        // only ever seeded once per session id (never overwritten here again), so a later
        // duck-induced live value never gets mistaken for a fresh user-set target. Mirrors
        // macOS's `gain_state.entry(id).or_default()` lazy-seeding for the same reason. Mute
        // needs no such shadow copy -- unlike volume, boosting never touches the real mute flag
        // (see this module's doc comment), so it's always safe to read live.
        for r in &raw {
            inner
                .target_volume
                .entry(r.id.clone())
                .or_insert(r.live_volume);
        }

        // Reconcile capture threads against the current priority-trigger list. Gated on ducking
        // actually being configured at all, so the (overwhelmingly common) ducking-off case stays
        // exactly as cheap as it was before this feature existed -- no capture activation
        // attempts, no extra threads, every poll tick.
        if inner.ducking_settings.enabled && !inner.ducking_settings.priority_triggers.is_empty() {
            let wanted: std::collections::HashSet<&str> = raw
                .iter()
                .filter(|r| {
                    r.is_active
                        && inner
                            .ducking_settings
                            .priority_triggers
                            .iter()
                            .any(|n| n == &r.display_name)
                })
                .map(|r| r.id.as_str())
                .collect();
            for r in &raw {
                if wanted.contains(r.id.as_str()) && !inner.captures.contains_key(&r.id) {
                    if let Some(pid) = pid_from_session_id(&r.id) {
                        log::info!(
                            "auto-duck: starting capture for priority app \"{}\" (pid {pid})",
                            r.display_name
                        );
                        inner.captures.insert(r.id.clone(), DuckCapture::new(pid));
                    }
                }
            }
            inner.captures.retain(|id, _| wanted.contains(id.as_str()));
        } else if !inner.captures.is_empty() {
            inner.captures.clear();
        }

        let any_triggering = inner.captures.values().any(|c| c.is_triggering());

        // Primed once per tick, not once per session below -- see `output_policy_factory`'s doc
        // comment for why the factory itself is cached across the backend's whole lifetime.
        output_policy_factory(&mut inner);

        let mut sessions = Vec::with_capacity(raw.len());
        for r in raw {
            let is_duck_trigger = inner.captures.get(&r.id).is_some_and(|c| c.is_triggering());
            // Ducked by *someone else* -- an app currently triggering never ducks itself, same
            // condition macOS's mixing pass applies.
            let is_ducked = any_triggering && !is_duck_trigger;
            let target = *inner.target_volume.get(&r.id).unwrap_or(&r.live_volume);
            let effective = if is_ducked {
                target * DUCK_GAIN_MULTIPLIER
            } else {
                target
            };

            // Only actually write through to WASAPI on a real transition -- comparing against
            // the last-applied state, not writing unconditionally every tick, so this doesn't
            // fight a live slider drag (which calls `set_volume` directly) or spam
            // `SetMasterVolume` for sessions nothing is currently ducking.
            let was_ducked = inner.applied_ducked.get(&r.id).copied().unwrap_or(false);
            if was_ducked != is_ducked {
                unsafe {
                    let _ = r.simple_volume.SetMasterVolume(effective, std::ptr::null());
                }
                inner.applied_ducked.insert(r.id.clone(), is_ducked);
            }

            let write_generation = inner.write_generations.get(&r.id).copied().unwrap_or(0);
            // Read live rather than trusting a locally-cached value -- the user can also change
            // this from Windows' own Settings panel directly, and the UI should reflect that too.
            // The *factory* is cached (see `output_policy_factory`'s doc comment); only the
            // per-session routing lookup itself runs fresh every tick.
            let output_device_raw = pid_from_session_id(&r.id).and_then(|pid| {
                inner.output_policy_factory.as_ref().and_then(|factory| {
                    windows_output_routing::get_session_output_device(factory, pid)
                })
            });
            // Debounced against a transient single-tick blip -- see `debounce_output_device`'s
            // doc comment.
            let output_device_id = debounce_output_device(&mut inner, &r.id, output_device_raw);
            // Cache this app's icon against its duck-trigger entry (if it has one), the first
            // time a real icon is actually resolved for it -- see `DuckingSettings::
            // priority_trigger_icons`'s doc comment for why the Settings UI needs this to keep
            // showing a real icon once the app closes or goes a whole run without making sound.
            // NOTE: written without access to a Windows machine -- please verify before trusting
            // it compiles/behaves as intended, same caveat as this file's other less-travelled
            // corners.
            if let Some(icon_bytes) = &r.icon_png {
                if icon_bytes.len() <= super::MAX_CACHEABLE_ICON_BYTES
                    && inner
                        .ducking_settings
                        .priority_triggers
                        .iter()
                        .any(|name| name == &r.display_name)
                    && inner
                        .ducking_settings
                        .priority_trigger_icons
                        .get(&r.display_name)
                        != Some(icon_bytes)
                {
                    inner
                        .ducking_settings
                        .priority_trigger_icons
                        .insert(r.display_name.clone(), icon_bytes.clone());
                    windows_ducking::save_settings(&inner.ducking_settings);
                }
            }
            sessions.push(AppSession {
                id: r.id,
                display_name: r.display_name,
                icon_png: r.icon_png,
                volume: target,
                effective_volume: effective,
                muted: r.muted,
                balance: r.balance,
                is_active: r.is_active,
                is_duck_trigger,
                is_ducked,
                write_generation,
                output_device_id,
            });
        }

        // Drop tracked state for sessions that no longer exist at all -- keeps these maps
        // bounded over a long Mixolume session that sees many different apps come and go,
        // matching macOS's `app_info_cache.retain` for the same reason. Owned strings (not
        // borrows of `sessions`) specifically so this can run before `sessions` is moved out by
        // the `Ok(...)` below.
        let live_ids: std::collections::HashSet<String> =
            sessions.iter().map(|s| s.id.clone()).collect();
        inner.target_volume.retain(|id, _| live_ids.contains(id));
        inner.applied_ducked.retain(|id, _| live_ids.contains(id));
        inner
            .output_device_raw
            .retain(|id, _| live_ids.contains(id));
        inner
            .output_device_confirmed
            .retain(|id, _| live_ids.contains(id));

        // Merges same-named sessions (e.g. Zoom's multiple processes) into one row -- deliberately
        // *after* the retains above, which need every individual pid's own `win-{pid}` id still
        // present in `live_ids` to keep that pid's own bookkeeping alive; the merged group id
        // itself never appears in any of those maps.
        let mut sessions = group_sessions_by_display_name(&mut inner, sessions);

        // `IAudioSessionEnumerator` (behind `enumerate_session_controls`) has no documented
        // ordering guarantee either, matching macOS's `kAudioHardwarePropertyProcessObjectList`
        // -- see the identical sort in `macos.rs`'s `list_sessions` for the full rationale (an
        // unstable order reshuffles the frontend's rendered list on poll ticks where nothing
        // user-visible changed, which its Framer Motion layout tracking reacts to as real
        // movement, confirmed live as the cause of sustained high frontend CPU once a second
        // session existed to reorder against).
        sessions.sort_by(|a, b| {
            a.display_name
                .cmp(&b.display_name)
                .then_with(|| a.id.cmp(&b.id))
        });

        Ok(sessions)
    }

    fn set_volume(&self, session_id: &str, volume: f32) -> Result<u64, MixerError> {
        let volume = clamp_volume(volume);
        // Held for this whole call, including every member's WASAPI write below -- not just the
        // target-volume bookkeeping. Releasing it early (an earlier version of this did) leaves
        // a real race against `list_sessions`'s own per-session duck-transition write: a slider
        // drag landing in the gap between "read `applied_ducked`" and "actually write the
        // volume" could have its un-ducked write land *after* `list_sessions` had already
        // (correctly) applied the ducked value for a trigger that started in between -- since
        // `list_sessions` only writes on a state *change*, that wrong value would then persist
        // until the duck state changed again, not just for one glitchy tick. Matches how
        // `macos.rs`'s `Inner`-guarded setters already hold their lock across their own
        // engine-mutating calls for the same reason.
        let mut inner = self.inner.lock().unwrap();
        // Fans out to every member pid if `session_id` is a group (e.g. Zoom's several
        // processes) -- see `resolve_member_pids`'s doc comment. Exactly one pid otherwise, so
        // this loop is a no-op-shaped identity for the overwhelmingly common case.
        let member_pids = resolve_member_pids(&inner, session_id)?;
        let mut applied_to_any = false;
        for pid in member_pids {
            let member_id = session_id_for(pid);
            inner.target_volume.insert(member_id.clone(), volume);
            let is_ducked = inner
                .applied_ducked
                .get(&member_id)
                .copied()
                .unwrap_or(false);
            let effective = if is_ducked {
                volume * DUCK_GAIN_MULTIPLIER
            } else {
                volume
            };

            // Best-effort per member: one process in a group failing (e.g. it just exited)
            // shouldn't block the volume from being applied to every other member that's still
            // there -- the call only fails outright if *none* of them could be reached.
            let Ok(control) = find_session_control(&mut inner, &member_id) else {
                continue;
            };
            unsafe {
                let Ok(simple_volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                if simple_volume
                    .SetMasterVolume(effective, std::ptr::null())
                    .is_ok()
                {
                    applied_to_any = true;
                }
            }
        }
        if !applied_to_any {
            return Err(MixerError::SessionNotFound(session_id.to_string()));
        }
        Ok(bump_generation(&mut inner, session_id))
    }

    fn set_muted(&self, session_id: &str, muted: bool) -> Result<u64, MixerError> {
        // Locked for the whole call, like `set_volume` -- see its own comment. Same fan-out
        // reasoning applies here too.
        let mut inner = self.inner.lock().unwrap();
        let member_pids = resolve_member_pids(&inner, session_id)?;
        let mut applied_to_any = false;
        for pid in member_pids {
            let member_id = session_id_for(pid);
            let Ok(control) = find_session_control(&mut inner, &member_id) else {
                continue;
            };
            unsafe {
                let Ok(simple_volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                if simple_volume
                    .SetMute(BOOL::from(muted), std::ptr::null())
                    .is_ok()
                {
                    applied_to_any = true;
                }
            }
        }
        if !applied_to_any {
            return Err(MixerError::SessionNotFound(session_id.to_string()));
        }
        Ok(bump_generation(&mut inner, session_id))
    }

    /// -1.0 (full left) to 1.0 (full right), applied via `IChannelAudioVolume` -- a separate
    /// session interface from `ISimpleAudioVolume`'s single master volume, obtained the same way
    /// (casting the session control), see
    /// https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-ichannelaudiovolume-setchannelvolume.
    /// Only meaningful for 2-channel (stereo) sessions -- a no-op (not an error) for anything
    /// else, since "left/right balance" doesn't have a sensible meaning for mono or
    /// surround-channel-count sessions.
    fn set_balance(&self, session_id: &str, balance: f32) -> Result<u64, MixerError> {
        // Locked for the whole call -- see `set_muted`'s matching comment. Same fan-out
        // reasoning applies here too.
        let mut inner = self.inner.lock().unwrap();
        let member_pids = resolve_member_pids(&inner, session_id)?;
        let mut applied_to_any = false;
        for pid in member_pids {
            let member_id = session_id_for(pid);
            let Ok(control) = find_session_control(&mut inner, &member_id) else {
                continue;
            };
            unsafe {
                let Ok(simple_volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                let volume = simple_volume.GetMasterVolume().unwrap_or(1.0);

                let Ok(channel_volume) = control.cast::<IChannelAudioVolume>() else {
                    continue;
                };
                if channel_volume.GetChannelCount().unwrap_or(0) != 2 {
                    // Not stereo -- nothing to do for this member, but that's not a failure.
                    applied_to_any = true;
                    continue;
                }
                let balance = balance.clamp(-1.0, 1.0);
                let left = volume * (1.0 - balance.max(0.0));
                let right = volume * (1.0 + balance.min(0.0));
                let _ = channel_volume.SetChannelVolume(0, left, std::ptr::null());
                let _ = channel_volume.SetChannelVolume(1, right, std::ptr::null());
                applied_to_any = true;
            }
        }
        if !applied_to_any {
            return Err(MixerError::SessionNotFound(session_id.to_string()));
        }
        Ok(bump_generation(&mut inner, session_id))
    }

    /// `app.exit()` calls `std::process::exit()` directly and does not run `Drop` for arbitrary
    /// managed state (same fact macOS's `shutdown` doc comment cites), so any session left
    /// mid-duck needs its real target volume restored here explicitly -- otherwise quitting while
    /// someone's being ducked would leave their volume stuck low with no auto-duck engine left
    /// running to ever bring it back up. Dropping every `DuckCapture` first (each one's `Drop`
    /// stops its thread) is what `captures` going out of scope would do anyway on a normal
    /// program exit, just synchronous and guaranteed here instead of racing process teardown.
    fn shutdown(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.captures.clear();

        let restores: Vec<(String, f32)> = inner
            .applied_ducked
            .iter()
            .filter(|(_, &was_ducked)| was_ducked)
            .filter_map(|(id, _)| inner.target_volume.get(id).map(|&v| (id.clone(), v)))
            .collect();
        for (id, target) in restores {
            if let Ok(control) = find_session_control(&mut inner, &id) {
                unsafe {
                    if let Ok(simple_volume) = control.cast::<ISimpleAudioVolume>() {
                        let _ = simple_volume.SetMasterVolume(target, std::ptr::null());
                    }
                }
            }
        }
    }

    fn get_ducking_settings(&self) -> DuckingSettings {
        self.inner.lock().unwrap().ducking_settings.clone()
    }

    fn set_ducking_enabled(&self, enabled: bool) -> Result<(), MixerError> {
        // Scoped so this guard drops before the possible `self.list_sessions()` call below --
        // that takes the same lock itself, and `std::sync::Mutex` isn't reentrant.
        let should_seed = {
            let mut inner = self.inner.lock().unwrap();
            let was_enabled = inner.ducking_settings.enabled;
            inner.ducking_settings.enabled = enabled;
            // First-ever enable (an empty list, not just "currently off"): pre-fill with
            // whichever well-known communication apps MiXolume has already seen making sound, so
            // the feature does something useful immediately instead of silently doing nothing
            // until the user manually finds and adds e.g. Discord themselves. Same
            // empty-list-as-signal simplification macOS's `set_ducking_enabled` uses -- see its
            // comment for why that's an acceptable tradeoff, not a real gap.
            enabled && !was_enabled && inner.ducking_settings.priority_triggers.is_empty()
        };

        if should_seed {
            // Best-effort: if this fails, the user can still add apps manually from Settings.
            if let Ok(sessions) = self.list_sessions() {
                let running_names: Vec<String> =
                    sessions.iter().map(|s| s.display_name.clone()).collect();
                let mut inner = self.inner.lock().unwrap();
                super::seed_priority_apps_from_well_known(
                    &mut inner.ducking_settings.priority_triggers,
                    WELL_KNOWN_COMMUNICATION_APPS,
                    &running_names,
                );
            }
        }

        let inner = self.inner.lock().unwrap();
        windows_ducking::save_settings(&inner.ducking_settings);
        Ok(())
    }

    fn set_duck_trigger_priority(
        &self,
        display_name: &str,
        is_priority: bool,
    ) -> Result<(), MixerError> {
        let mut inner = self.inner.lock().unwrap();
        super::toggle_priority_trigger(&mut inner.ducking_settings, display_name, is_priority);
        windows_ducking::save_settings(&inner.ducking_settings);
        Ok(())
    }

    fn output_routing_supported(&self) -> bool {
        true
    }

    fn list_output_devices(&self) -> Result<Vec<OutputDevice>, MixerError> {
        let devices = windows_output_routing::list_output_devices()?;
        // NOTE: written without access to a Windows machine to compile/run this against -- please
        // verify on real hardware before trusting it, same caveat already noted elsewhere in this
        // file for code written the same way.
        //
        // Mirrors a fix already verified on macOS: `IAudioPolicyConfigFactory`'s persisted per-app
        // override has no liveness check of its own -- `get_session_output_device` just returns
        // whatever's stored, even for a device that's since been unplugged -- so without this, a
        // session stayed "routed" to a since-unplugged device indefinitely, both in what
        // `list_sessions` reports and in the OS's own persisted policy store. Piggybacks on this
        // call's own ~2s poll cadence (`spawn_output_devices_poll_loop` in `lib.rs`) rather than
        // adding a new one; clearing the OS's own persisted value here is enough on its own --
        // `list_sessions`'s next ~150ms tick reads it back fresh via `get_session_output_device`
        // and will correctly see `None`, no need to also touch `output_device_raw`/
        // `output_device_confirmed` directly.
        let live_device_ids: std::collections::HashSet<&str> =
            devices.iter().map(|d| d.id.as_str()).collect();
        let mut inner = self.inner.lock().unwrap();
        let stale_session_ids: Vec<String> = inner
            .output_device_confirmed
            .iter()
            .filter_map(|(session_id, device_id)| {
                let device_id = device_id.as_deref()?;
                (!live_device_ids.contains(device_id)).then(|| session_id.clone())
            })
            .collect();
        let mut stale_pids: Vec<u32> = Vec::new();
        for session_id in &stale_session_ids {
            if let Ok(member_pids) = resolve_member_pids(&inner, session_id) {
                stale_pids.extend(member_pids);
            }
        }
        if !stale_pids.is_empty() {
            if let Some(factory) = output_policy_factory(&mut inner) {
                for pid in stale_pids {
                    let _ = windows_output_routing::set_session_output_device(factory, pid, None);
                }
            }
        }
        Ok(devices)
    }

    fn set_session_output_device(
        &self,
        session_id: &str,
        device_id: Option<&str>,
    ) -> Result<(), MixerError> {
        let mut inner = self.inner.lock().unwrap();
        // Fans out to every member pid if `session_id` is a group -- routing "Zoom" to
        // headphones should mean every one of its processes, matching the single-app mental
        // model the rest of this backend's grouping already applies to volume/mute/balance.
        let member_pids = resolve_member_pids(&inner, session_id)?;
        let factory = output_policy_factory(&mut inner).ok_or_else(|| {
            MixerError::Platform("failed to activate IAudioPolicyConfigFactory".to_string())
        })?;
        let mut applied_to_any = false;
        for pid in member_pids {
            if windows_output_routing::set_session_output_device(factory, pid, device_id).is_ok() {
                applied_to_any = true;
            }
        }
        if !applied_to_any {
            return Err(MixerError::Platform(
                "failed to set output device for any member".to_string(),
            ));
        }
        Ok(())
    }
}

/// Best-effort read of a session's current left/right balance from its live
/// `IChannelAudioVolume` channel levels, inverting the same linear pan law `set_balance` writes
/// with (see its doc comment). `None` for non-stereo sessions or if the interface can't be
/// obtained -- callers should treat that as "centered" rather than an error, matching how the
/// rest of `list_sessions` already falls back to `unwrap_or(...)` defaults for best-effort reads.
unsafe fn read_balance(control: &IAudioSessionControl2) -> Option<f32> {
    let channel_volume: IChannelAudioVolume = control.cast().ok()?;
    if channel_volume.GetChannelCount().ok()? != 2 {
        return None;
    }
    let left = channel_volume.GetChannelVolume(0).ok()?;
    let right = channel_volume.GetChannelVolume(1).ok()?;
    Some(if right >= left {
        if right <= 0.0 {
            0.0
        } else {
            1.0 - left / right
        }
    } else if left <= 0.0 {
        0.0
    } else {
        right / left - 1.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual smoke test against the real WASAPI session list on whatever machine runs it --
    /// not a unit test (no fixture, depends on real audio actually playing), so it's `#[ignore]`d
    /// by default. Run with something audible playing: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn manual_list_real_sessions() {
        let backend = WindowsMixerBackend::new();
        let sessions = backend
            .list_sessions()
            .expect("list_sessions should succeed");
        for s in &sessions {
            println!(
                "{} (id={}, volume={:.2}, muted={}, active={})",
                s.display_name, s.id, s.volume, s.muted, s.is_active
            );
        }
        assert!(
            !sessions.is_empty(),
            "expected at least one audio session -- make sure something is actually playing sound"
        );
    }

    #[test]
    fn session_id_round_trips_through_pid_parsing() {
        assert_eq!(pid_from_session_id(&session_id_for(1234)), Some(1234));
    }

    #[test]
    fn pid_from_session_id_rejects_foreign_ids() {
        assert_eq!(pid_from_session_id("linux-7"), None);
        assert_eq!(pid_from_session_id("win-not-a-number"), None);
    }
}
