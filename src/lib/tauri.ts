import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ===== TYPES =====

// Mirrors the Rust `AppSession` struct (serde `rename_all = "camelCase"`).
// `iconPng` arrives as a plain byte array (serde's default `Vec<u8>` JSON
// encoding), not a data URL - see `src/lib/iconUrl.ts` for the conversion.
export interface AppSession {
  id: string;
  displayName: string;
  iconPng: number[] | null;
  /** The volume the user set -- what the slider drags from, unaffected by auto-duck. */
  volume: number;
  /** What's actually coming out right now -- lower than `volume` while `isDucked` is true. */
  effectiveVolume: number;
  muted: boolean;
  /** -1.0 (full left) to 1.0 (full right), 0.0 centered. */
  balance: number;
  isActive: boolean;
  /** Auto-duck currently has this app pegged as why everything else is quieter. macOS/Windows only. */
  isDuckTrigger: boolean;
  /** Auto-duck is currently lowering this app's volume because another app is triggering it. */
  isDucked: boolean;
  /** Monotonically increasing per-session counter, bumped by the backend on every
   * `setVolume`/`setMuted`/`setBalance` call -- lets the frontend recognize (and discard) a push
   * whose data was read before its own most recent write landed, no matter how delayed that
   * push's own delivery turns out to be. See the Rust `AppSession::write_generation` doc comment
   * for the full rationale (a confirmed, occasionally 100ms+, delay in the backend's own
   * `emit()` call that a fixed-duration protection window can't reliably outlast). */
  writeGeneration: number;
  /** The output device this session is routed to, or `null` when it's following the system
   * default. `null` on every backend that doesn't implement output routing yet -- check
   * `outputRoutingSupported()` before showing a device picker at all. */
  outputDeviceId: string | null;
}

// Mirrors the Rust `OutputDevice` struct. `id` is backend-specific and opaque here -- only ever
// round-tripped back through `setSessionOutputDevice`.
export interface OutputDevice {
  id: string;
  name: string;
}

/**
 * One session as it arrives over `sessions-changed` -- the same shape as `AppSession` except
 * that `iconPng` is *absent* (not null) when the backend already sent that id's icon in an
 * earlier push and it hasn't changed since. See `PushedSession` in `lib.rs` for why re-sending
 * an unchanged icon is expensive enough to be worth avoiding, and `resolvePushedIcons` in
 * `mixer-store.ts` for where the omitted value is filled back in.
 */
export type SessionPush = Omit<AppSession, "iconPng"> & {
  iconPng?: number[] | null;
};

// Substring of the Rust error returned while Screen & System Audio Recording permission hasn't
// been granted yet (see `screen_capture_permission::ensure_granted` in macos.rs). Screen Recording
// is one of the few macOS permission categories that only takes effect after a full app relaunch
// -- granting it while MiXolume is already running will not make sessions start appearing on
// their own, no matter how long the app keeps polling. The UI needs to tell the user this
// explicitly rather than silently showing an empty list forever.
const PERMISSION_ERROR_MARKER = "screen & system audio recording permission";

export const isPermissionError = (error: unknown): boolean =>
  typeof error === "string" && error.includes(PERMISSION_ERROR_MARKER);

// Mirrors the Rust `UpdateCheckOutcome` enum (serde `tag = "status"`, `rename_all =
// "camelCase"`). A found update is downloaded and installed before this resolves -- the install
// only takes effect on the next launch, so `installed` here means "ready", not "applied yet".
export type UpdateCheckOutcome =
  | { status: "upToDate" }
  | { status: "installed"; version: string };

// Mirrors the Rust `DuckingSettings` struct. Opt-in, not opt-out: `priorityTriggers` is the list
// of apps explicitly allowed to trigger a duck -- an empty list means the feature does nothing
// yet, not "everything triggers." Implemented on macOS and Windows; Linux's backend doesn't
// override the trait's default methods, which report `{ enabled: false, priorityTriggers: [] }`
// and ignore writes, so the Settings UI can call these unconditionally without checking the
// platform itself (it still checks `duckingSupported()` before showing the toggle at all).
export interface DuckingSettings {
  enabled: boolean;
  priorityTriggers: string[];
  /** PNG icon bytes for each name in `priorityTriggers`, keyed the same way -- captured the first
   * time that app was seen actively producing sound. Absent for a trigger never seen active since
   * being added; the Settings UI falls back to a currently-live session's icon when one's
   * available, and to this cached one otherwise. */
  priorityTriggerIcons: Record<string, number[]>;
}

// ===== TAURI COMMANDS =====

export const listSessions = () => invoke<AppSession[]>("list_sessions");

// Each resolves with the session's new `writeGeneration` -- see `AppSession.writeGeneration`'s
// doc comment. `mixer-store.ts` records it immediately so it can recognize a later push that
// still reflects data read before this write landed, regardless of how long that push itself
// took to arrive.
export const setVolume = (sessionId: string, volume: number) =>
  invoke<number>("set_volume", { sessionId, volume });

export const setMuted = (sessionId: string, muted: boolean) =>
  invoke<number>("set_muted", { sessionId, muted });

export const setBalance = (sessionId: string, balance: number) =>
  invoke<number>("set_balance", { sessionId, balance });

// The highest volume percent the current backend allows a session to be set to -- 100 everywhere
// except macOS (200, boosted like VLC's own past-100% slider). Drives the volume slider's `max`.
export const maxVolumePercent = () => invoke<number>("max_volume_percent");

// Routed through a Rust command rather than calling `getCurrentWindow().startDragging()`
// directly -- see `begin_window_drag`'s doc comment in lib.rs for why starting a drag needs the
// same hide-on-blur guard as showing the window does.
export const beginWindowDrag = () => invoke<void>("begin_window_drag");

export const checkForUpdates = () =>
  invoke<UpdateCheckOutcome>("check_for_updates");

export const getDuckingSettings = () =>
  invoke<DuckingSettings>("get_ducking_settings");

// Auto-duck needs per-app raw audio content (to tell speech from music) -- macOS gets that via
// Core Audio process taps, Windows via WASAPI process-loopback capture. Linux has no such backend
// yet and reports `false` here so the Settings UI can hide the toggle instead of showing a
// control that no-ops silently.
export const duckingSupported = () => invoke<boolean>("ducking_supported");

export const setDuckingEnabled = (enabled: boolean) =>
  invoke<void>("set_ducking_enabled", { enabled });

export const setDuckTriggerPriority = (displayName: string, isPriority: boolean) =>
  invoke<void>("set_duck_trigger_priority", { displayName, isPriority });

// Currently Windows only (via the undocumented `IAudioPolicyConfigFactory` WinRT API, the same
// one behind Windows' own Settings > Sound > Volume mixer per-app device picker). Reports `false`
// elsewhere so the UI can hide the device picker instead of showing a control that no-ops.
export const outputRoutingSupported = () =>
  invoke<boolean>("output_routing_supported");

export const listOutputDevices = () =>
  invoke<OutputDevice[]>("list_output_devices");

export const setSessionOutputDevice = (
  sessionId: string,
  deviceId: string | null,
) => invoke<void>("set_session_output_device", { sessionId, deviceId });

// ===== EVENTS =====

// Rust pushes the updated session list whenever it changes (on its own polling interval - not
// something the frontend controls). Every session is present every time; only `iconPng` may be
// omitted - see `SessionPush`.
export const listenToSessionsChanged = (
  callback: (sessions: SessionPush[]) => void,
) =>
  listen<SessionPush[]>("sessions-changed", (event) => {
    callback(event.payload);
  });

// Rust pushes a fresh output-device list whenever a device is plugged/unplugged (its own slower
// ~2s polling interval, separate from the session list's -- see `OUTPUT_DEVICES_POLL_INTERVAL`
// in lib.rs), so the picker's dropdown options stay live instead of frozen at whatever was
// plugged in when the app started.
export const listenToOutputDevicesChanged = (
  callback: (devices: OutputDevice[]) => void,
) =>
  listen<OutputDevice[]>("output-devices-changed", (event) => {
    callback(event.payload);
  });
