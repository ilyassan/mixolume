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

use std::collections::HashMap;
use std::sync::Mutex;

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2, IChannelAudioVolume,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
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
use super::{clamp_volume, AppSession, AudioMixerBackend, DuckingSettings, MixerError};

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
    /// `SetMasterVolume` write only happens on an actual duck-state transition, not every single
    /// ~150ms poll regardless of whether anything changed (which would mean constantly writing a
    /// volume even for sessions nothing is currently ducking).
    applied_ducked: HashMap<String, bool>,
    /// Per-session `write_generation` (see `AppSession::write_generation`'s doc comment), bumped
    /// by every `set_volume`/`set_muted`/`set_balance` call.
    write_generations: HashMap<String, u64>,
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
            }),
        }
    }
}

impl Default for WindowsMixerBackend {
    fn default() -> Self {
        Self::new()
    }
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

/// Every non-expired, non-system audio session control currently registered with the default
/// render endpoint's session manager.
fn enumerate_session_controls() -> windows::core::Result<Vec<IAudioSessionControl2>> {
    unsafe {
        // Ignore the return value: RPC_E_CHANGED_MODE / S_FALSE both mean "some form of COM is
        // already initialized on this thread", which is fine for our purposes.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let device_enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = device_enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let session_manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let session_enum: IAudioSessionEnumerator = session_manager.GetSessionEnumerator()?;
        let count = session_enum.GetCount()?;

        let mut controls = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let control: IAudioSessionControl = session_enum.GetSession(i)?;
            let control2: IAudioSessionControl2 = control.cast()?;
            controls.push(control2);
        }
        Ok(controls)
    }
}

fn session_id_for(pid: u32) -> String {
    format!("win-{pid}")
}

fn pid_from_session_id(id: &str) -> Option<u32> {
    id.strip_prefix("win-").and_then(|s| s.parse::<u32>().ok())
}

fn find_session_control(session_id: &str) -> Result<IAudioSessionControl2, MixerError> {
    let target_pid = pid_from_session_id(session_id)
        .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;
    let controls = enumerate_session_controls().map_err(|e| MixerError::Platform(e.to_string()))?;
    unsafe {
        for control in controls {
            if control.GetProcessId().unwrap_or(0) == target_pid {
                return Ok(control);
            }
        }
    }
    Err(MixerError::SessionNotFound(session_id.to_string()))
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
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        let controls =
            enumerate_session_controls().map_err(|e| MixerError::Platform(e.to_string()))?;

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

        let mut raw = Vec::new();
        unsafe {
            for control in controls {
                // `IsSystemSoundsSession` returns a raw HRESULT: S_OK (0) means "yes", S_FALSE
                // (1) means "no" -- both are non-negative, so `.is_ok()` (which only checks
                // "not a failure code") is true for EVERY session and would skip all of them.
                // We must compare against S_OK specifically.
                if control.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    continue;
                }

                let pid = match control.GetProcessId() {
                    Ok(pid) if pid != 0 && pid != own_pid => pid,
                    _ => continue,
                };

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

                let (display_name, icon_png) = resolve_process_info(pid);

                raw.push(RawSession {
                    id: session_id_for(pid),
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

        let mut inner = self.inner.lock().unwrap();

        // Seed each newly-seen session's tracked target volume from its current live value --
        // only ever seeded once per session id (never overwritten here again), so a later
        // duck-induced live value never gets mistaken for a fresh user-set target. Mirrors
        // macOS's `gain_state.entry(id).or_default()` lazy-seeding for the same reason.
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
        // Held for this whole call, including the WASAPI write below -- not just the
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
        inner.target_volume.insert(session_id.to_string(), volume);
        let is_ducked = inner
            .applied_ducked
            .get(session_id)
            .copied()
            .unwrap_or(false);
        let to_apply = if is_ducked {
            volume * DUCK_GAIN_MULTIPLIER
        } else {
            volume
        };

        let control = find_session_control(session_id)?;
        unsafe {
            let simple_volume: ISimpleAudioVolume = control
                .cast()
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            simple_volume
                .SetMasterVolume(to_apply, std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))?;
        }
        Ok(bump_generation(&mut inner, session_id))
    }

    fn set_muted(&self, session_id: &str, muted: bool) -> Result<u64, MixerError> {
        let control = find_session_control(session_id)?;
        unsafe {
            let simple_volume: ISimpleAudioVolume = control
                .cast()
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            simple_volume
                .SetMute(BOOL::from(muted), std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))?;
        }
        let mut inner = self.inner.lock().unwrap();
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
        let control = find_session_control(session_id)?;
        unsafe {
            let simple_volume: ISimpleAudioVolume = control
                .cast()
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            let volume = simple_volume.GetMasterVolume().unwrap_or(1.0);

            let channel_volume: IChannelAudioVolume = control
                .cast()
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            if channel_volume.GetChannelCount().unwrap_or(0) != 2 {
                let mut inner = self.inner.lock().unwrap();
                return Ok(bump_generation(&mut inner, session_id));
            }
            let balance = balance.clamp(-1.0, 1.0);
            let left = volume * (1.0 - balance.max(0.0));
            let right = volume * (1.0 + balance.min(0.0));
            channel_volume
                .SetChannelVolume(0, left, std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            channel_volume
                .SetChannelVolume(1, right, std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))?;
        }
        let mut inner = self.inner.lock().unwrap();
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
            if let Ok(control) = find_session_control(&id) {
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
        super::toggle_priority_trigger(
            &mut inner.ducking_settings.priority_triggers,
            display_name,
            is_priority,
        );
        windows_ducking::save_settings(&inner.ducking_settings);
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
