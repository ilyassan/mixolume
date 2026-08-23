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
  volume: number;
  muted: boolean;
  isActive: boolean;
}

// Substring of the Rust error returned while Screen & System Audio Recording permission hasn't
// been granted yet (see `screen_capture_permission::ensure_granted` in macos.rs). Screen Recording
// is one of the few macOS permission categories that only takes effect after a full app relaunch
// -- granting it while Mixolume is already running will not make sessions start appearing on
// their own, no matter how long the app keeps polling. The UI needs to tell the user this
// explicitly rather than silently showing an empty list forever.
const PERMISSION_ERROR_MARKER = "screen & system audio recording permission";

export const isPermissionError = (error: unknown): boolean =>
  typeof error === "string" && error.includes(PERMISSION_ERROR_MARKER);

// ===== TAURI COMMANDS =====

export const listSessions = () => invoke<AppSession[]>("list_sessions");

export const setVolume = (sessionId: string, volume: number) =>
  invoke<void>("set_volume", { sessionId, volume });

export const setMuted = (sessionId: string, muted: boolean) =>
  invoke<void>("set_muted", { sessionId, muted });

// ===== EVENTS =====

// Rust pushes the full, updated session list whenever it changes (on its own
// polling interval - not something the frontend controls).
export const listenToSessionsChanged = (
  callback: (sessions: AppSession[]) => void,
) =>
  listen<AppSession[]>("sessions-changed", (event) => {
    callback(event.payload);
  });
