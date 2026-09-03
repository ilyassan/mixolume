//! Per-app output device routing: letting one app play through headphones while another plays
//! through speakers, simultaneously -- the same feature Windows' own Settings -> Sound -> Volume
//! mixer panel exposes per-app, just surfaced through Mixolume's UI instead.
//!
//! The mechanism is `IAudioPolicyConfigFactory`, an undocumented but long-stable WinRT interface
//! (it backs that native Settings panel itself, and is what EarTrumpet/SoundSwitch/Audio Router
//! use for the same feature) -- confirmed against EarTrumpet's own MIT-licensed source
//! (github.com/File-New-Project/EarTrumpet) and verified end-to-end against real hardware via a
//! standalone proof-of-concept before this file was first written, not just read about.
//!
//! Real, non-obvious things this took to get right, each confirmed against EarTrumpet's actual
//! source (not just its docs) rather than assumed:
//!
//! 1. **Two IIDs, by Windows version.** Microsoft changed the interface for Windows 11's
//!    redesigned audio settings. `real_build_number()` reads the true build via `RtlGetVersion`
//!    (NOT `GetVersionExW`, which lies about the OS version without an app manifest declaring
//!    Windows 10/11 compatibility) and [`policy_config_variant`] picks the matching IID.
//!
//! 2. **The device id format is not what `IMMDevice::GetId()` returns.** The bare MMDevice
//!    endpoint id (e.g. `{0.0.0.00000000}.{guid}`) is rejected outright with `E_INVALIDARG` --
//!    confirmed live: an empty string and a deliberately-bogus string both failed the exact same
//!    way as the real endpoint id, which is what pinned this down to "wrong format" rather than
//!    "wrong pid" or "wrong device" or a marshaling bug. The API wants the full PnP device
//!    interface path instead: `\\?\SWD#MMDEVAPI#<mmdevice-id>#{e6327cad-dcec-4949-ae8a-991e976a79d2}`
//!    (that trailing GUID is `DEVINTERFACE_AUDIO_RENDER`) -- see [`render_device_interface_path`].
//!
//! 3. **A write must cover both `eConsole` and `eMultimedia`, not just one.** This is the actual
//!    root cause of a real bug this file went through: an earlier version only wrote/read
//!    `eConsole`, which round-tripped fine in isolation (`Set` then `Get` for the same role
//!    matched) but didn't reliably stick in real use -- a freshly-picked device would show
//!    selected for a moment, then silently revert to "System default". Different apps resolve
//!    their default render endpoint via different roles (most commonly `eConsole` or
//!    `eMultimedia`), so an override that only covers one role is an *incomplete* override, not a
//!    smaller-but-still-valid one. Confirmed by reading EarTrumpet's actual working
//!    implementation (`AudioPolicyConfigService.SetDefaultEndPoint`/`GetDefaultEndPoint`), which
//!    writes `eMultimedia` then `eConsole` on every set, and reads both back on every get -- never
//!    just one. [`PolicyConfigFactory::set`]/[`get`] now do the same.
//!
//! 4. **Every call must run on a thread that has joined the WinRT apartment**, not just whichever
//!    thread happened to activate the factory once. `windows.rs` caches one factory instance for
//!    the backend's whole lifetime, but its `Set`/`Get` calls arrive from different OS threads
//!    over time (Tauri's own command-dispatch pool for a `set_session_output_device` command; the
//!    async poll loop, which tokio can resume on a different worker thread after every `.await`).
//!    A thread that never itself called `CoInitializeEx`/`RoInitialize` making a raw vtable call
//!    into this interface was confirmed live to be a second real, independent cause of the same
//!    "looks set, then reverts" symptom. [`PolicyConfigFactory::set`]/[`get`]/[`clear_all`] each
//!    join defensively on every call now, the same way every other COM-touching function in this
//!    module already did.
//!
//! An empty/absent device id (used to reset a session back to the system default) is passed as a
//! genuinely NULL `HSTRING`, not an `HSTRING` wrapping a zero-length string -- though per
//! Microsoft's own documented HSTRING semantics those two are byte-identical anyway (an HSTRING
//! representing the empty string *is* NULL, by convention: `WindowsCreateString` with a
//! zero-length source is documented to produce the NULL handle), so `HSTRING::from("")` already
//! produces exactly what EarTrumpet's explicit `IntPtr.Zero` does.
//!
//! `IAudioPolicyConfigFactory` is `IInspectable`-based (a WinRT interface), not a plain `IUnknown`
//! COM interface -- its vtable preamble is 6 methods (`QueryInterface`/`AddRef`/`Release`/
//! `GetIids`/`GetRuntimeClassName`/`GetTrustLevel`), not the usual 3. After that: 19 unrelated
//! methods (chat-app/volume-group APIs this file has no use for) that still have to occupy a
//! vtable slot each to keep the offsets correct, then the 3 methods this file actually calls --
//! this exact slot count (19) is confirmed directly against EarTrumpet's own
//! `IAudioPolicyConfigFactoryVariantFor21H2` C# interface declaration, which lists exactly 19
//! `__incomplete__*` placeholder methods before `SetPersistedDefaultAudioEndpoint`.

use std::mem::transmute_copy;

use windows::core::{Interface, Result as WinResult, HRESULT, HSTRING};
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceCollection,
    IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::WinRT::{RoGetActivationFactory, RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

use super::{MixerError, OutputDevice};

// ==================================================================================================
// OS version detection.
// ==================================================================================================

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(version_info: *mut OSVERSIONINFOW) -> i32;
}

/// The real Windows build number, bypassing `GetVersionExW`'s app-manifest-gated lie.
fn real_build_number() -> u32 {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = RtlGetVersion(&mut info);
    }
    info.dwBuildNumber
}

/// Build 21390 is Windows 10 21H2 -- the boundary EarTrumpet's own source uses (confirmed against
/// `Environment.OSVersion.IsAtLeast(OSVersions.Version21H2)` in its `AudioPolicyConfigFactory.Create()`).
/// Every Windows 11 build is well above this too, so a single `>=` comparison correctly covers
/// both "Windows 10 21H2+" and "any Windows 11".
const WINDOWS_21H2_BUILD: u32 = 21390;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyConfigVariant {
    Downlevel,
    Modern,
}

/// Pure and unit-testable on its own, independent of actually reading the real OS version.
fn policy_config_variant(build_number: u32) -> PolicyConfigVariant {
    if build_number >= WINDOWS_21H2_BUILD {
        PolicyConfigVariant::Modern
    } else {
        PolicyConfigVariant::Downlevel
    }
}

// ==================================================================================================
// IAudioPolicyConfigFactory -- see module doc comment for the full rationale/verification.
// ==================================================================================================

/// Both roles a write must cover -- see the module doc comment's point 3. Iterated together by
/// every caller that sets or reads a session's override, so there's exactly one place a future
/// change (e.g. also covering `eCommunications`, which EarTrumpet itself does not) would need to
/// touch.
const ENDPOINT_ROLES: [ERole; 2] = [eConsole, eMultimedia];

macro_rules! define_policy_config_factory {
    ($name:ident, $vtbl:ident, $iid:literal) => {
        windows::core::imp::define_interface!($name, $vtbl, $iid);
        windows::core::imp::interface_hierarchy!(
            $name,
            windows::core::IUnknown,
            windows::core::IInspectable
        );

        // `pub`: a private type here would leak through `$name`'s own `Interface::Vtable`
        // associated type, which must be at least as visible as `$name` itself.
        #[repr(C)]
        pub struct $vtbl {
            base__: windows::core::IInspectable_Vtbl,
            // 19 methods (chat-application preference / volume-group APIs) this file never
            // calls -- still one opaque vtable slot each, purely to keep the offsets below
            // correct. See the module doc comment for how this exact count was confirmed.
            _unused_1: unsafe extern "system" fn() -> HRESULT,
            _unused_2: unsafe extern "system" fn() -> HRESULT,
            _unused_3: unsafe extern "system" fn() -> HRESULT,
            _unused_4: unsafe extern "system" fn() -> HRESULT,
            _unused_5: unsafe extern "system" fn() -> HRESULT,
            _unused_6: unsafe extern "system" fn() -> HRESULT,
            _unused_7: unsafe extern "system" fn() -> HRESULT,
            _unused_8: unsafe extern "system" fn() -> HRESULT,
            _unused_9: unsafe extern "system" fn() -> HRESULT,
            _unused_10: unsafe extern "system" fn() -> HRESULT,
            _unused_11: unsafe extern "system" fn() -> HRESULT,
            _unused_12: unsafe extern "system" fn() -> HRESULT,
            _unused_13: unsafe extern "system" fn() -> HRESULT,
            _unused_14: unsafe extern "system" fn() -> HRESULT,
            _unused_15: unsafe extern "system" fn() -> HRESULT,
            _unused_16: unsafe extern "system" fn() -> HRESULT,
            _unused_17: unsafe extern "system" fn() -> HRESULT,
            _unused_18: unsafe extern "system" fn() -> HRESULT,
            _unused_19: unsafe extern "system" fn() -> HRESULT,
            set_persisted_default_audio_endpoint: unsafe extern "system" fn(
                this: *mut core::ffi::c_void,
                process_id: u32,
                flow: EDataFlow,
                role: ERole,
                device_id: std::mem::MaybeUninit<HSTRING>,
            ) -> HRESULT,
            get_persisted_default_audio_endpoint: unsafe extern "system" fn(
                this: *mut core::ffi::c_void,
                process_id: u32,
                flow: EDataFlow,
                role: ERole,
                device_id: *mut *mut u16,
            ) -> HRESULT,
            clear_all_persisted_application_default_endpoints:
                unsafe extern "system" fn(this: *mut core::ffi::c_void) -> HRESULT,
        }

        impl $name {
            /// # Safety
            /// `device_id` must be either empty/null (reset to system default) or a full device
            /// interface path from [`render_device_interface_path`] -- the bare MMDevice
            /// endpoint id is rejected, see the module doc comment.
            unsafe fn set_persisted_default_audio_endpoint(
                &self,
                process_id: u32,
                role: ERole,
                device_id: &str,
            ) -> WinResult<()> {
                let hstring = HSTRING::from(device_id);
                let result = (Interface::vtable(self).set_persisted_default_audio_endpoint)(
                    transmute_copy(self),
                    process_id,
                    eRender,
                    role,
                    std::mem::MaybeUninit::new(std::ptr::read(&hstring)),
                )
                .ok();
                // The callee doesn't take ownership of a by-value WinRT HSTRING parameter passed
                // this way (confirmed against EarTrumpet's own `SetDefaultEndPoint`, which passes
                // the exact same `hstring` handle across *two* calls, one per role -- if the
                // callee freed it on the first call, the second would be using a dangling
                // handle) -- forget the local copy so it isn't double-freed alongside the one
                // just passed through the vtable call.
                std::mem::forget(hstring);
                result
            }

            unsafe fn get_persisted_default_audio_endpoint(
                &self,
                process_id: u32,
                role: ERole,
            ) -> WinResult<String> {
                let mut raw: *mut u16 = std::ptr::null_mut();
                (Interface::vtable(self).get_persisted_default_audio_endpoint)(
                    transmute_copy(self),
                    process_id,
                    eRender,
                    role,
                    &mut raw,
                )
                .ok()?;
                if raw.is_null() {
                    return Ok(String::new());
                }
                // Ownership transfers to us here -- `HSTRING`'s `Drop` frees it correctly since
                // this is exactly the representation `windows_core::HSTRING` wraps.
                let hstring = std::mem::transmute::<*mut u16, HSTRING>(raw);
                Ok(hstring.to_string_lossy())
            }

            /// Emergency-recovery escape hatch: resets *every* app's persisted per-app output
            /// device override back to following the system default, in one call. Exposed via
            /// [`PolicyConfigFactory::clear_all`] for a future "reset all routing" UI action, and
            /// used once as a one-off diagnostic to undo state a broken version of this file had
            /// already persisted on a real machine.
            unsafe fn clear_all_persisted_application_default_endpoints(&self) -> WinResult<()> {
                (Interface::vtable(self).clear_all_persisted_application_default_endpoints)(
                    transmute_copy(self),
                )
                .ok()
            }
        }
    };
}

define_policy_config_factory!(
    IAudioPolicyConfigFactoryDownlevel,
    IAudioPolicyConfigFactoryDownlevel_Vtbl,
    0x2a59116d_6c4f_45e0_a74f_707e3fef9258
);
define_policy_config_factory!(
    IAudioPolicyConfigFactoryModern,
    IAudioPolicyConfigFactoryModern_Vtbl,
    0xab3d4648_e242_459f_b02f_541c70306324
);

const RUNTIME_CLASS_NAME: &str = "Windows.Media.Internal.AudioPolicyConfig";

/// Thin enum over the two IID-selected variants (identical vtable shape, see the module doc
/// comment) so callers don't need to branch on OS version at every call site.
///
/// `pub(super)`, not private: `windows.rs` caches one of these in `Inner` for the lifetime of the
/// backend instead of calling [`Self::activate`] (a real `RoGetActivationFactory` WinRT
/// activation, not free) on every single session on every ~150ms poll tick.
pub(super) enum PolicyConfigFactory {
    Downlevel(IAudioPolicyConfigFactoryDownlevel),
    Modern(IAudioPolicyConfigFactoryModern),
}

// SAFETY: like `windows_audio::ActivationResult`, this wraps WinRT/COM interface pointers that
// are `!Send`/`!Sync` by default (Rust can't verify apartment-threading rules), which is overly
// strict here specifically: every call site below joins the multithreaded apartment
// (`CoInitializeEx`/`RoInitialize`, idempotent if already joined) before touching the interface,
// so a genuinely free-threaded MTA object is used exactly as its threading model allows, as long
// as concurrent calls on the same instance are externally synchronized -- which they are, since
// the only real caller (`windows.rs`) only ever touches its cached instance from inside `Inner`'s
// `Mutex`-guarded single-accessor-at-a-time discipline.
unsafe impl Send for PolicyConfigFactory {}

impl PolicyConfigFactory {
    pub(super) fn activate() -> Result<Self, MixerError> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = RoInitialize(RO_INIT_MULTITHREADED);

            let class_name = HSTRING::from(RUNTIME_CLASS_NAME);
            match policy_config_variant(real_build_number()) {
                PolicyConfigVariant::Modern => {
                    RoGetActivationFactory::<IAudioPolicyConfigFactoryModern>(&class_name)
                        .map(Self::Modern)
                        .map_err(|e| MixerError::Platform(e.to_string()))
                }
                PolicyConfigVariant::Downlevel => {
                    RoGetActivationFactory::<IAudioPolicyConfigFactoryDownlevel>(&class_name)
                        .map(Self::Downlevel)
                        .map_err(|e| MixerError::Platform(e.to_string()))
                }
            }
        }
    }

    /// Joins the WinRT multithreaded apartment on whatever thread is calling right now --
    /// cheap/idempotent if it already has (see the module doc comment's point 4). Every method
    /// below calls this before touching the vtable, since none can assume it's running on the
    /// same thread that happened to call [`Self::activate`].
    fn ensure_apartment_joined() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
    }

    fn set_one_role(&self, pid: u32, role: ERole, device_id: &str) -> WinResult<()> {
        unsafe {
            match self {
                Self::Downlevel(f) => f.set_persisted_default_audio_endpoint(pid, role, device_id),
                Self::Modern(f) => f.set_persisted_default_audio_endpoint(pid, role, device_id),
            }
        }
    }

    fn get_one_role(&self, pid: u32, role: ERole) -> WinResult<String> {
        unsafe {
            match self {
                Self::Downlevel(f) => f.get_persisted_default_audio_endpoint(pid, role),
                Self::Modern(f) => f.get_persisted_default_audio_endpoint(pid, role),
            }
        }
    }

    /// Sets `pid`'s persisted default render endpoint for *every* role in [`ENDPOINT_ROLES`], not
    /// just one -- see the module doc comment's point 3 for why a single-role write is an
    /// incomplete override, not a smaller-but-valid one. Reports the first role's failure if more
    /// than one fails, but still attempts every role regardless -- matching EarTrumpet's own
    /// unconditional back-to-back calls, which don't short-circuit on the first either.
    pub(super) fn set(&self, pid: u32, device_id: &str) -> Result<(), MixerError> {
        Self::ensure_apartment_joined();
        let mut first_error = None;
        for role in ENDPOINT_ROLES {
            if let Err(e) = self.set_one_role(pid, role, device_id) {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(MixerError::Platform(e.to_string())),
            None => Ok(()),
        }
    }

    /// The device `pid` is overridden to, checked across every role in [`ENDPOINT_ROLES`] (they're
    /// always written together by [`Self::set`], but read independently here rather than assuming
    /// they can never drift -- e.g. if the user changed this from Windows' own Settings panel,
    /// which is not guaranteed to write every role this file does). Empty if none has an override.
    pub(super) fn get(&self, pid: u32) -> Result<String, MixerError> {
        Self::ensure_apartment_joined();
        for role in ENDPOINT_ROLES {
            match self.get_one_role(pid, role) {
                Ok(value) if !value.is_empty() => return Ok(value),
                _ => continue,
            }
        }
        Ok(String::new())
    }

    /// Emergency-recovery-only escape hatch, not called from any normal per-session code path --
    /// see the vtable method's own doc comment. Kept around (unlike a truly temporary tool) as a
    /// future "reset all routing" UI action's building block.
    #[allow(dead_code)]
    pub(super) fn clear_all(&self) -> Result<(), MixerError> {
        Self::ensure_apartment_joined();
        let result = unsafe {
            match self {
                Self::Downlevel(f) => f.clear_all_persisted_application_default_endpoints(),
                Self::Modern(f) => f.clear_all_persisted_application_default_endpoints(),
            }
        };
        result.map_err(|e| MixerError::Platform(e.to_string()))
    }
}

/// `DEVINTERFACE_AUDIO_RENDER` -- the well-known device-interface-class GUID that, appended to an
/// MMDevice endpoint id inside the `\\?\SWD#MMDEVAPI#...` PnP path format, is what
/// `IAudioPolicyConfigFactory` actually expects as a device id (see the module doc comment for
/// how this was found: the bare MMDevice id was rejected identically to genuine garbage).
const DEVINTERFACE_AUDIO_RENDER: &str = "{e6327cad-dcec-4949-ae8a-991e976a79d2}";

/// Builds the full device interface path from a plain MMDevice endpoint id (what
/// `IMMDevice::GetId()` returns). Pure and unit-testable without any live COM/WinRT calls.
pub(super) fn render_device_interface_path(mmdevice_id: &str) -> String {
    format!("\\\\?\\SWD#MMDEVAPI#{mmdevice_id}#{DEVINTERFACE_AUDIO_RENDER}")
}

// ==================================================================================================
// Device enumeration -- standard, documented WASAPI, no undocumented-interface risk here.
// ==================================================================================================

/// PKEY_Device_FriendlyName -- documented in the Core Audio API headers, just not exposed as a
/// named constant by the `windows` crate's `Win32_Devices_Properties`/`Win32_UI_Shell_*` bindings
/// at the pinned version, so it's written out directly.
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

pub fn list_output_devices() -> Result<Vec<OutputDevice>, MixerError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let device_enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| MixerError::Platform(e.to_string()))?;
        let collection: IMMDeviceCollection = device_enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| MixerError::Platform(e.to_string()))?;
        let count = collection
            .GetCount()
            .map_err(|e| MixerError::Platform(e.to_string()))?;

        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let Ok(device) = collection.Item(i) else {
                continue;
            };
            let Some((id, name)) = describe_device(&device) else {
                continue;
            };
            devices.push(OutputDevice { id, name });
        }
        Ok(devices)
    }
}

/// Best-effort: a device this fails for (can't read its id or friendly name) is simply omitted
/// from the list rather than failing the whole enumeration, matching this backend's existing
/// "one bad entry shouldn't take down the rest" pattern (see `resolve_process_info` in
/// `windows.rs`).
unsafe fn describe_device(device: &IMMDevice) -> Option<(String, String)> {
    let mmdevice_id = device.GetId().ok()?.to_string().ok()?;
    let id = render_device_interface_path(&mmdevice_id);
    let name = friendly_name(device).unwrap_or_else(|| mmdevice_id.clone());
    Some((id, name))
}

unsafe fn friendly_name(device: &IMMDevice) -> Option<String> {
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let variant = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok()?;
    PropVariantToStringAlloc(&variant).ok()?.to_string().ok()
}

// ==================================================================================================
// Setting/reading a session's routed device.
// ==================================================================================================

/// Routes `pid`'s audio to `device_id` (a full path from [`list_output_devices`]), or resets it
/// to the system default when `device_id` is `None` -- an empty-string device id is what
/// `IAudioPolicyConfigFactory` itself treats as "no override" (confirmed live, and matches
/// documented HSTRING semantics -- see the module doc comment).
///
/// Takes an already-[`PolicyConfigFactory::activate`]d factory rather than activating one itself
/// -- `windows.rs` caches a single instance for the backend's lifetime (see `Inner`'s doc
/// comment) instead of paying for a real WinRT activation on every call.
pub(super) fn set_session_output_device(
    factory: &PolicyConfigFactory,
    pid: u32,
    device_id: Option<&str>,
) -> Result<(), MixerError> {
    factory.set(pid, device_id.unwrap_or(""))
}

/// The device `pid` is currently routed to, or `None` if it's following the system default.
/// Reads live OS state rather than trusting a locally-cached value, since the user can also
/// change this from Windows' own Settings panel directly -- Mixolume's UI should reflect that,
/// not just what it itself last wrote. Same cached-factory rationale as
/// [`set_session_output_device`].
pub(super) fn get_session_output_device(factory: &PolicyConfigFactory, pid: u32) -> Option<String> {
    let value = factory.get(pid).ok()?;
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // policy_config_variant -- the version-threshold branch, no live OS call needed.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn windows_10_below_21h2_uses_downlevel_variant() {
        assert_eq!(policy_config_variant(19041), PolicyConfigVariant::Downlevel);
        assert_eq!(
            policy_config_variant(WINDOWS_21H2_BUILD - 1),
            PolicyConfigVariant::Downlevel
        );
    }

    #[test]
    fn windows_10_21h2_and_later_use_modern_variant() {
        assert_eq!(
            policy_config_variant(WINDOWS_21H2_BUILD),
            PolicyConfigVariant::Modern
        );
        // Any real Windows 11 build (all well above 22000) also takes this branch.
        assert_eq!(policy_config_variant(26100), PolicyConfigVariant::Modern);
    }

    // ---------------------------------------------------------------------------------------
    // render_device_interface_path -- pure string formatting, verified against the exact
    // format confirmed live on real hardware (see the module doc comment).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn builds_the_confirmed_working_path_format() {
        let path =
            render_device_interface_path("{0.0.0.00000000}.{af71ede6-3095-4d76-b139-366aa8e4c2d7}");
        assert_eq!(
            path,
            "\\\\?\\SWD#MMDEVAPI#{0.0.0.00000000}.{af71ede6-3095-4d76-b139-366aa8e4c2d7}#{e6327cad-dcec-4949-ae8a-991e976a79d2}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // ENDPOINT_ROLES -- the actual fix: both roles, matching EarTrumpet's own working
    // implementation exactly (see the module doc comment's point 3).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn covers_both_console_and_multimedia_roles() {
        assert!(ENDPOINT_ROLES.contains(&eConsole));
        assert!(ENDPOINT_ROLES.contains(&eMultimedia));
        assert_eq!(ENDPOINT_ROLES.len(), 2);
    }
}
