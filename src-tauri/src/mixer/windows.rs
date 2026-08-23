//! Windows backend: WASAPI audio sessions via the documented Core Audio COM interfaces.
//!
//! `IMMDeviceEnumerator` -> default render endpoint -> `IAudioSessionManager2` ->
//! `IAudioSessionEnumerator` -> one `IAudioSessionControl2` per app producing sound.
//! `IAudioSessionControl2` gives us the owning process id; `ISimpleAudioVolume` (obtained by
//! casting the same session control) gives us get/set volume + mute. No elevated privileges,
//! no driver — see PLAN.md section 2.

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

use super::{clamp_volume, AppSession, AudioMixerBackend, MixerError};

pub struct WindowsMixerBackend;

impl WindowsMixerBackend {
    pub fn new() -> Self {
        Self
    }
}

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

impl AudioMixerBackend for WindowsMixerBackend {
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        let controls =
            enumerate_session_controls().map_err(|e| MixerError::Platform(e.to_string()))?;

        let mut sessions = Vec::new();
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
                    Ok(pid) if pid != 0 => pid,
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
                let volume = simple_volume.GetMasterVolume().unwrap_or(0.0);
                let muted = simple_volume
                    .GetMute()
                    .map(|b| b.as_bool())
                    .unwrap_or(false);
                let balance = read_balance(&control).unwrap_or(0.0);

                let (display_name, icon_png) = resolve_process_info(pid);

                sessions.push(AppSession {
                    id: session_id_for(pid),
                    display_name,
                    icon_png,
                    volume,
                    muted,
                    balance,
                    is_active: state == AudioSessionStateActive,
                });
            }
        }
        Ok(sessions)
    }

    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError> {
        let control = find_session_control(session_id)?;
        unsafe {
            let simple_volume: ISimpleAudioVolume = control
                .cast()
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            simple_volume
                .SetMasterVolume(clamp_volume(volume), std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))
        }
    }

    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError> {
        let control = find_session_control(session_id)?;
        unsafe {
            let simple_volume: ISimpleAudioVolume = control
                .cast()
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            simple_volume
                .SetMute(BOOL::from(muted), std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))
        }
    }

    /// -1.0 (full left) to 1.0 (full right), applied via `IChannelAudioVolume` -- a separate
    /// session interface from `ISimpleAudioVolume`'s single master volume, obtained the same way
    /// (casting the session control), see
    /// https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-ichannelaudiovolume-setchannelvolume.
    /// Only meaningful for 2-channel (stereo) sessions -- a no-op (not an error) for anything
    /// else, since "left/right balance" doesn't have a sensible meaning for mono or
    /// surround-channel-count sessions.
    fn set_balance(&self, session_id: &str, balance: f32) -> Result<(), MixerError> {
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
                return Ok(());
            }
            let balance = balance.clamp(-1.0, 1.0);
            let left = volume * (1.0 - balance.max(0.0));
            let right = volume * (1.0 + balance.min(0.0));
            channel_volume
                .SetChannelVolume(0, left, std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))?;
            channel_volume
                .SetChannelVolume(1, right, std::ptr::null())
                .map_err(|e| MixerError::Platform(e.to_string()))
        }
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
        let sessions = backend.list_sessions().expect("list_sessions should succeed");
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
