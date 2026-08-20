//! macOS [`AudioMixerBackend`] -- talks to an independently-installed BackgroundMusic
//! (`kyleneideck/BackgroundMusic`) `BGMDriver` virtual audio device over the *public*
//! `AudioObjectGetPropertyData` / `AudioObjectSetPropertyData` Core Audio HAL client API.
//!
//! # THIS FILE HAS NOT BEEN COMPILED OR RUN
//!
//! Written on a Windows machine with no access to Xcode, a Mac, or the Core Audio frameworks.
//! It is real, best-effort Core Audio HAL client code, transcribed carefully from Apple's
//! documented `AudioObjectGetPropertyData`/`AudioObjectSetPropertyData` call shape and from
//! BackgroundMusic's public header, but it needs a first compile + smoke test on a real Mac with
//! BackgroundMusic installed before anyone trusts it. See `src-tauri/macos-driver/README.md` for
//! the legal reasoning (why this talks to an externally-installed driver instead of vendoring
//! BackgroundMusic's GPLv2 source) and for the open product question about `BGMApp`'s role that
//! this file deliberately does NOT resolve.
//!
//! Known risk areas a Mac-equipped contributor should double check first, since they were
//! written from memory of the `core-foundation` 0.9.x API shape rather than from compiled/checked
//! source:
//! - [`CFDictionary::find`] -- the lookup method name/signature on `core_foundation::dictionary`
//!   may differ slightly by crate version; if it doesn't compile, the fix is almost certainly
//!   just renaming/adjusting this one call, not restructuring the type-erased
//!   `CFDictionary<CFString, CFType>` + `downcast::<T>()` approach itself.
//! - `kAudioObjectPropertyElementMaster` -- newer macOS SDKs renamed this to
//!   `kAudioObjectPropertyElementMain` (kept as a deprecated alias with the same value, `0`).
//!   Whichever one `coreaudio-sys` 0.2's bindgen run picked up is the one that will compile.
//! - Icon/display-name resolution is NOT implemented (see [`AppVolumeEntry::to_app_session`]) --
//!   BGMDriver's property only ever hands us a pid and/or a bundle identifier string, never a
//!   human name or an icon. Resolving those needs AppKit/LaunchServices
//!   (`NSRunningApplication`, `LSCopyApplicationURLsForBundleIdentifier`, ...), which isn't in
//!   this crate's dependency list yet.
//! - `is_active` is NOT a real signal -- see the doc comment on
//!   [`AppVolumeEntry::to_app_session`]. BGMDriver only exposes a device-wide audible state, not
//!   a per-app one, so this field is currently always `true` for any app BGMDriver has an entry
//!   for. Do not treat it as ground truth.
//! - Mute is synthesized locally -- see the doc comment on [`MacosMixerBackend::set_muted`].
//!   BGMDriver's per-app property has no separate mute bit, only a single relative-volume number,
//!   so "muted" here means "we told BGMDriver to set volume to 0 and we remember what it was
//!   before" purely on Mixolume's side.
//!
//! ## What was actually confirmed by reading BackgroundMusic's source (2026-08-20)
//!
//! `SharedSource/BGM_Types.h` (as of the `master` branch on GitHub) defines:
//! - `kBGMDeviceUID = "BGMDevice"` -- the CFString UID of BGMDriver's main virtual output device.
//! - `kAudioDeviceCustomPropertyAppVolumes = 'apvs'` -- **the one property that actually gives
//!   per-app control**: "A CFArray of CFDictionaries that each contain an app's pid, bundle ID
//!   and volume relative to other running apps." Each dictionary can contain:
//!   - `"rvol"` (`kBGMAppVolumesKey_RelativeVolume`): `CFNumber<SInt32>`, `0..=100`.
//!   - `"ppos"` (`kBGMAppVolumesKey_PanPosition`): `CFNumber<SInt32>`, `-100..=100`.
//!   - `"pid"` (`kBGMAppVolumesKey_ProcessID`): `CFNumber`, may be omitted if `"bid"` is present.
//!   - `"bid"` (`kBGMAppVolumesKey_BundleID`): `CFString`, may be omitted if `"pid"` is present.
//!   Per BackgroundMusic's `DEVELOPING.md`: "When you change an app's volume, BGMApp sends the
//!   new volume to BGMDriver, which applies the app volumes by modifying the apps' audio data
//!   directly." That send is nothing more than an `AudioObjectSetPropertyData` call on this
//!   property against the address `{ 'apvs', kAudioObjectPropertyScopeGlobal,
//!   kAudioObjectPropertyElementMain }` -- there is nothing BGMApp-specific about the mechanism,
//!   which is exactly why this file can target BGMDriver directly. **So: per-app volume control
//!   via a public-shaped HAL property genuinely exists here** -- this is not the "no such
//!   property exists" case.
//! - `kAudioDeviceCustomPropertyDeviceAudibleState = 'daud'` -- device-wide only (silent / silent
//!   except music-player app / audible), not per app. Not used for per-app state here.
//! - Other custom properties in the same header (`'mppi'`/`'mpbi'` music-player pid/bundle,
//!   `'runo'` running-elsewhere flag, `'bgct'` enabled-output-controls, `'dblg'` debug logging)
//!   are unrelated to per-app volume and are not used by this file.
//!
//! What's genuinely *unconfirmed* (no Mac to check on): whether the `'apvs'` array is populated
//! by BGMDriver proactively as new client processes start playing audio (the likely mechanism,
//! since there is no separate "list current audio clients" property in this header), and exactly
//! when/whether entries are ever removed again. `list_sessions` below assumes "present in the
//! array" is a reasonable proxy for "known", but cannot promise it matches "currently playing."

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::string::CFStringRef;

use coreaudio_sys::{
    kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, OSStatus,
};

use super::{clamp_volume, AppSession, AudioMixerBackend, MixerError};

// ---------------------------------------------------------------------------------------------
// Constants transcribed from BackgroundMusic's SharedSource/BGM_Types.h (fetched 2026-08-20 from
// https://github.com/kyleneideck/BackgroundMusic/blob/master/SharedSource/BGM_Types.h). Mixolume
// does not vendor that header or any other BackgroundMusic source -- these are just the public
// string/fourCC *values* needed to address an independently-installed BGMDriver instance over the
// standard Core Audio HAL client API. See src-tauri/macos-driver/README.md.
// ---------------------------------------------------------------------------------------------

/// `kBGMDeviceUID` -- the CFString UID BGMDriver registers its main virtual output device under.
const BGM_DEVICE_UID: &str = "BGMDevice";

/// Pack 4 ASCII bytes into the classic Mac OS `FourCharCode` u32 the same way Apple's own headers
/// spell literals like `'aVol'` in C. `AudioObjectPropertySelector` (and friends) are just `UInt32`
/// under the hood.
const fn fourcc(bytes: [u8; 4]) -> u32 {
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | (bytes[3] as u32)
}

/// `kAudioDeviceCustomPropertyAppVolumes` ('apvs') -- the one property in BGM_Types.h that
/// exposes per-application volume/pan control. Value type: `CFArray` of `CFDictionary`, one dict
/// per app BGMDriver knows about.
const K_BGM_APP_VOLUMES: AudioObjectPropertySelector =
    fourcc(*b"apvs") as AudioObjectPropertySelector;

// Dictionary keys inside each element of the kAudioDeviceCustomPropertyAppVolumes CFArray.
const K_APP_VOLUMES_KEY_RELATIVE_VOLUME: &str = "rvol"; // CFNumber<SInt32>, 0..=100
const K_APP_VOLUMES_KEY_PAN_POSITION: &str = "ppos"; // CFNumber<SInt32>, -100..=100
const K_APP_VOLUMES_KEY_PROCESS_ID: &str = "pid"; // CFNumber, may be omitted if "bid" present
const K_APP_VOLUMES_KEY_BUNDLE_ID: &str = "bid"; // CFString, may be omitted if "pid" present

const APP_RELATIVE_VOLUME_MIN_RAW: i32 = 0;
const APP_RELATIVE_VOLUME_MAX_RAW: i32 = 100;

/// A single element of the `kAudioDeviceCustomPropertyAppVolumes` array, in a Rust-native shape.
#[derive(Debug, Clone)]
struct AppVolumeEntry {
    pid: Option<i32>,
    bundle_id: Option<String>,
    relative_volume_raw: i32,
    pan_raw: i32,
}

impl AppVolumeEntry {
    /// What we use as [`AppSession::id`]: prefer the pid (numeric, unique per running instance),
    /// fall back to the bundle id if BGMDriver only gave us that.
    fn session_id(&self) -> String {
        match (self.pid, &self.bundle_id) {
            (Some(pid), _) => pid.to_string(),
            (None, Some(bid)) => bid.clone(),
            (None, None) => "unknown".to_string(),
        }
    }

    fn to_app_session(&self) -> AppSession {
        let volume_raw = self
            .relative_volume_raw
            .clamp(APP_RELATIVE_VOLUME_MIN_RAW, APP_RELATIVE_VOLUME_MAX_RAW);
        let volume = volume_raw as f32 / APP_RELATIVE_VOLUME_MAX_RAW as f32;

        AppSession {
            id: self.session_id(),
            // BGMDriver only ever gives us a bundle id and/or a bare pid here -- resolving that
            // into a human-readable display name (and an icon) needs AppKit/LaunchServices
            // (e.g. NSRunningApplication, or LSCopyApplicationURLsForBundleIdentifier +
            // reading the bundle's Info.plist / .icns), which is NOT wired up in this crate yet.
            // TODO(macos): resolve display_name/icon_png properly; this is a placeholder.
            display_name: self
                .bundle_id
                .clone()
                .unwrap_or_else(|| format!("pid {}", self.pid.unwrap_or(-1))),
            icon_png: None,
            volume,
            // BGMDriver's per-app property has no separate mute bit -- only this single
            // relative-volume number. So "muted" is indistinguishable on the wire from "user
            // manually set volume to zero." This is an honest approximation, not a fact
            // BGMDriver told us. See `MacosMixerBackend::set_muted` for the fuller story.
            muted: volume <= 0.0,
            // UNVERIFIED: BGMDriver does not expose a per-app "is this app making sound right
            // now" flag anywhere in BGM_Types.h -- only a device-wide audible state
            // ('daud': silent / silent-except-music-player / audible). Treating "has an entry
            // in the apvs array" as "active" is optimistic and has not been confirmed against
            // real BGMDriver behavior (in particular: it's unknown whether entries are ever
            // removed from the array once an app stops playing, or whether they persist for the
            // life of the device). Do not treat this field as reliable yet.
            is_active: true,
        }
    }
}

fn check_status(status: OSStatus, what: &str) -> Result<(), MixerError> {
    if status == 0 {
        Ok(())
    } else {
        Err(MixerError::Platform(format!(
            "{what} failed with OSStatus {status}"
        )))
    }
}

/// Every `AudioObjectID` currently known to the system HAL (`kAudioHardwarePropertyDevices` on
/// `kAudioObjectSystemObject`). Unlike the custom BGM properties below, this one's value is a
/// flat C array of `AudioObjectID` (plain `UInt32`s), not a CF object, so it uses the
/// size-then-fetch pattern directly rather than the CF wrap-under-create-rule dance.
fn list_all_device_ids() -> Result<Vec<AudioObjectID>, MixerError> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let system_object: AudioObjectID = kAudioObjectSystemObject;

    let mut size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(system_object, &address, 0, std::ptr::null(), &mut size)
    };
    check_status(
        status,
        "AudioObjectGetPropertyDataSize(kAudioHardwarePropertyDevices)",
    )?;

    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    let mut ids: Vec<AudioObjectID> = vec![0; count];
    let mut actual_size = size;
    let status = unsafe {
        AudioObjectGetPropertyData(
            system_object,
            &address,
            0,
            std::ptr::null(),
            &mut actual_size,
            ids.as_mut_ptr() as *mut c_void,
        )
    };
    check_status(
        status,
        "AudioObjectGetPropertyData(kAudioHardwarePropertyDevices)",
    )?;
    Ok(ids)
}

/// Fetch a Core-Foundation-object-typed property. Per Apple's documented semantics for CF-typed
/// HAL properties, the HAL hands the caller one retain on the returned object ("copy"/"get"
/// semantics) -- the raw pointer returned here must be wrapped with exactly one
/// `TCFType::wrap_under_create_rule` call by the caller, never `wrap_under_get_rule`.
fn get_property_cf_raw(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Result<CFTypeRef, MixerError> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    };

    let mut raw: CFTypeRef = std::ptr::null();
    let mut size = std::mem::size_of::<CFTypeRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            &mut raw as *mut CFTypeRef as *mut c_void,
        )
    };
    check_status(status, "AudioObjectGetPropertyData")?;
    if raw.is_null() {
        return Err(MixerError::Platform(
            "Core Audio returned a null property value".to_string(),
        ));
    }
    Ok(raw)
}

fn get_device_uid(device_id: AudioObjectID) -> Result<String, MixerError> {
    let raw = get_property_cf_raw(
        device_id,
        kAudioDevicePropertyDeviceUID,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let cf_string = unsafe { CFString::wrap_under_create_rule(raw as CFStringRef) };
    Ok(cf_string.to_string())
}

/// Find BGMDriver's virtual device by its known UID (`kBGMDeviceUID` = `"BGMDevice"`). Returns
/// `Err` (not panicking, not faking a device) if BackgroundMusic isn't installed / its driver
/// plugin isn't loaded -- that's an expected, common state (most users won't have it installed),
/// not a bug.
fn find_bgm_device() -> Result<AudioObjectID, MixerError> {
    let device_ids = list_all_device_ids()?;
    for id in device_ids {
        if let Ok(uid) = get_device_uid(id) {
            if uid == BGM_DEVICE_UID {
                return Ok(id);
            }
        }
    }
    Err(MixerError::Platform(format!(
        "no Core Audio device with UID '{BGM_DEVICE_UID}' was found -- is BackgroundMusic's \
         BGMDriver installed and loaded? See src-tauri/macos-driver/README.md."
    )))
}

/// Read the current `kAudioDeviceCustomPropertyAppVolumes` array off the given device.
///
/// RISK (unverified): the `CFDictionary::find` call below is the single highest-risk line in
/// this file re: exact `core-foundation` 0.9.x API surface -- if it doesn't compile as spelled,
/// the fix is almost certainly a small rename/adjustment here, not a rethink of the overall
/// "type-erase to `CFType`, then `downcast::<T>()` per expected key" approach.
fn read_app_volumes(device_id: AudioObjectID) -> Result<Vec<AppVolumeEntry>, MixerError> {
    let raw = get_property_cf_raw(
        device_id,
        K_BGM_APP_VOLUMES,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let array: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(raw as CFArrayRef) };

    let mut entries = Vec::with_capacity(array.len() as usize);
    for item in array.iter() {
        match item.downcast::<CFDictionary<CFString, CFType>>() {
            Some(dict) => entries.push(parse_app_volume_dict(&dict)),
            // Unexpected element shape (shouldn't happen per the documented property format) --
            // skip defensively rather than panic on what is, after all, another process's data.
            None => continue,
        }
    }
    Ok(entries)
}

fn parse_app_volume_dict(dict: &CFDictionary<CFString, CFType>) -> AppVolumeEntry {
    let pid = dict
        .find(CFString::new(K_APP_VOLUMES_KEY_PROCESS_ID))
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_i32());
    let bundle_id = dict
        .find(CFString::new(K_APP_VOLUMES_KEY_BUNDLE_ID))
        .and_then(|v| v.downcast::<CFString>())
        .map(|s| s.to_string());
    let relative_volume_raw = dict
        .find(CFString::new(K_APP_VOLUMES_KEY_RELATIVE_VOLUME))
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_i32())
        .unwrap_or(APP_RELATIVE_VOLUME_MAX_RAW);
    let pan_raw = dict
        .find(CFString::new(K_APP_VOLUMES_KEY_PAN_POSITION))
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_i32())
        .unwrap_or(0);

    AppVolumeEntry {
        pid,
        bundle_id,
        relative_volume_raw,
        pan_raw,
    }
}

fn build_app_volume_dict(entry: &AppVolumeEntry) -> CFDictionary<CFString, CFType> {
    let mut pairs: Vec<(CFString, CFType)> = vec![
        (
            CFString::new(K_APP_VOLUMES_KEY_RELATIVE_VOLUME),
            CFNumber::from(entry.relative_volume_raw).as_CFType(),
        ),
        (
            CFString::new(K_APP_VOLUMES_KEY_PAN_POSITION),
            CFNumber::from(entry.pan_raw).as_CFType(),
        ),
    ];
    if let Some(pid) = entry.pid {
        pairs.push((
            CFString::new(K_APP_VOLUMES_KEY_PROCESS_ID),
            CFNumber::from(pid).as_CFType(),
        ));
    }
    if let Some(bid) = &entry.bundle_id {
        pairs.push((
            CFString::new(K_APP_VOLUMES_KEY_BUNDLE_ID),
            CFString::new(bid).as_CFType(),
        ));
    }
    CFDictionary::from_CFType_pairs(&pairs)
}

/// Write the *entire* `kAudioDeviceCustomPropertyAppVolumes` array back to the device.
///
/// IMPORTANT: this property is set as a whole array, not per-entry. Callers MUST read-modify-
/// write (read the full current array, mutate just the one entry they care about, then write the
/// full array back) -- never construct an array containing only the one app being changed, or
/// every other app's volume/pan on the device will be silently reset to whatever
/// `build_app_volume_dict`'s defaults are. Both `set_volume` and `set_muted` below follow this
/// read-modify-write pattern.
fn write_app_volumes(
    device_id: AudioObjectID,
    entries: &[AppVolumeEntry],
) -> Result<(), MixerError> {
    let cf_dicts: Vec<CFDictionary<CFString, CFType>> =
        entries.iter().map(build_app_volume_dict).collect();
    let cf_array: CFArray<CFDictionary<CFString, CFType>> = CFArray::from_CFTypes(&cf_dicts);

    let address = AudioObjectPropertyAddress {
        mSelector: K_BGM_APP_VOLUMES,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };

    let raw_array_ref = cf_array.as_concrete_TypeRef();
    let status = unsafe {
        AudioObjectSetPropertyData(
            device_id,
            &address,
            0,
            std::ptr::null(),
            std::mem::size_of::<CFArrayRef>() as u32,
            &raw_array_ref as *const CFArrayRef as *const c_void,
        )
    };
    check_status(
        status,
        "AudioObjectSetPropertyData(kAudioDeviceCustomPropertyAppVolumes)",
    )
}

/// macOS backend: an [`AudioMixerBackend`] that reads/writes BGMDriver's
/// `kAudioDeviceCustomPropertyAppVolumes` HAL property on an independently-installed
/// BackgroundMusic virtual device. See the module doc comment for what's confirmed vs.
/// unverified, and `src-tauri/macos-driver/README.md` for why BackgroundMusic isn't vendored
/// into this repo and for the open question about whether Mixolume also needs `BGMApp` running
/// alongside it.
pub struct MacosMixerBackend {
    /// Resolved lazily and cached: BGMDriver's `AudioObjectID` is stable for as long as the
    /// driver plugin stays loaded by `coreaudiod`, so there's no need to re-enumerate every call.
    /// `None` means "not yet resolved" -- NOT "confirmed absent" (BackgroundMusic could be
    /// installed after Mixolume starts), so every call attempts resolution again if the cache is
    /// empty rather than caching a negative result.
    device_id: Mutex<Option<AudioObjectID>>,
    /// Best-effort memory of "volume before mute", keyed by [`AppSession::id`]. Exists only
    /// because BGMDriver's wire format has no separate mute bit -- see `set_muted`.
    pre_mute_volume: Mutex<HashMap<String, f32>>,
}

impl MacosMixerBackend {
    pub fn new() -> Self {
        Self {
            device_id: Mutex::new(None),
            pre_mute_volume: Mutex::new(HashMap::new()),
        }
    }

    fn resolve_device_id(&self) -> Result<AudioObjectID, MixerError> {
        {
            let cached = self.device_id.lock().unwrap();
            if let Some(id) = *cached {
                return Ok(id);
            }
        }
        let id = find_bgm_device()?;
        *self.device_id.lock().unwrap() = Some(id);
        Ok(id)
    }
}

impl AudioMixerBackend for MacosMixerBackend {
    fn list_sessions(&self) -> Result<Vec<AppSession>, MixerError> {
        let device_id = self.resolve_device_id()?;
        let entries = read_app_volumes(device_id)?;
        Ok(entries.iter().map(AppVolumeEntry::to_app_session).collect())
    }

    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError> {
        let device_id = self.resolve_device_id()?;
        let mut entries = read_app_volumes(device_id)?;
        let entry = entries
            .iter_mut()
            .find(|e| e.session_id() == session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;

        let clamped = clamp_volume(volume);
        entry.relative_volume_raw = (clamped * APP_RELATIVE_VOLUME_MAX_RAW as f32).round() as i32;
        if clamped > 0.0 {
            self.pre_mute_volume
                .lock()
                .unwrap()
                .insert(session_id.to_string(), clamped);
        }
        write_app_volumes(device_id, &entries)
    }

    /// Approximates mute/unmute on top of a wire format that has no mute bit.
    ///
    /// BGMDriver's `apvs` property only carries a single relative-volume number per app -- there
    /// is no separate flag meaning "muted" as opposed to "user actually set volume to zero." So:
    /// - Muting sets the raw relative volume to 0 and remembers the pre-mute value **in this
    ///   process's memory only** (`pre_mute_volume`).
    /// - Unmuting restores that remembered value (defaulting to full volume if we never observed
    ///   a non-zero value for this session, e.g. across a Mixolume restart).
    ///
    /// This is lossy: it will not survive a Mixolume restart, and it will not agree with mute
    /// state set by any other BGM client (e.g. a real BGMApp instance, if the user also has one
    /// installed and running) unless that client happens to encode mute the same way. This is a
    /// known, documented limitation of the property itself, not a bug in this implementation.
    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError> {
        let device_id = self.resolve_device_id()?;
        let mut entries = read_app_volumes(device_id)?;
        let entry = entries
            .iter_mut()
            .find(|e| e.session_id() == session_id)
            .ok_or_else(|| MixerError::SessionNotFound(session_id.to_string()))?;

        if muted {
            let current = entry.relative_volume_raw as f32 / APP_RELATIVE_VOLUME_MAX_RAW as f32;
            if current > 0.0 {
                self.pre_mute_volume
                    .lock()
                    .unwrap()
                    .insert(session_id.to_string(), current);
            }
            entry.relative_volume_raw = APP_RELATIVE_VOLUME_MIN_RAW;
        } else {
            let restored = self
                .pre_mute_volume
                .lock()
                .unwrap()
                .get(session_id)
                .copied()
                .unwrap_or(1.0);
            entry.relative_volume_raw =
                (restored * APP_RELATIVE_VOLUME_MAX_RAW as f32).round() as i32;
        }
        write_app_volumes(device_id, &entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the pure-logic helpers are tested here -- everything else in this file needs a real
    // Core Audio HAL and an installed BGMDriver, neither of which exist in CI for this crate yet.

    #[test]
    fn fourcc_packs_bytes_big_endian_like_apple_c_literals() {
        // 'apvs' as a C multi-char literal is 0x61707673.
        assert_eq!(fourcc(*b"apvs"), 0x6170_7673);
    }

    #[test]
    fn volume_to_raw_round_trips_at_the_extremes() {
        let zero = AppVolumeEntry {
            pid: Some(1),
            bundle_id: None,
            relative_volume_raw: APP_RELATIVE_VOLUME_MIN_RAW,
            pan_raw: 0,
        };
        let full = AppVolumeEntry {
            pid: Some(1),
            bundle_id: None,
            relative_volume_raw: APP_RELATIVE_VOLUME_MAX_RAW,
            pan_raw: 0,
        };
        assert_eq!(zero.to_app_session().volume, 0.0);
        assert_eq!(full.to_app_session().volume, 1.0);
    }

    #[test]
    fn session_id_prefers_pid_over_bundle_id() {
        let entry = AppVolumeEntry {
            pid: Some(42),
            bundle_id: Some("com.example.app".to_string()),
            relative_volume_raw: APP_RELATIVE_VOLUME_MAX_RAW,
            pan_raw: 0,
        };
        assert_eq!(entry.session_id(), "42");
    }

    #[test]
    fn session_id_falls_back_to_bundle_id_without_pid() {
        let entry = AppVolumeEntry {
            pid: None,
            bundle_id: Some("com.example.app".to_string()),
            relative_volume_raw: APP_RELATIVE_VOLUME_MAX_RAW,
            pan_raw: 0,
        };
        assert_eq!(entry.session_id(), "com.example.app");
    }
}
