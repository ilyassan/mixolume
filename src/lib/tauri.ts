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
}

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
}

// ===== TAURI COMMANDS =====

export const listSessions = () => invoke<AppSession[]>("list_sessions");

export const setVolume = (sessionId: string, volume: number) =>
  invoke<void>("set_volume", { sessionId, volume });

export const setMuted = (sessionId: string, muted: boolean) =>
  invoke<void>("set_muted", { sessionId, muted });

export const setBalance = (sessionId: string, balance: number) =>
  invoke<void>("set_balance", { sessionId, balance });

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

// ===== EVENTS =====

// Rust pushes the full, updated session list whenever it changes (on its own
// polling interval - not something the frontend controls).
export const listenToSessionsChanged = (
  callback: (sessions: AppSession[]) => void,
) =>
  listen<AppSession[]>("sessions-changed", (event) => {
    callback(event.payload);
  });
