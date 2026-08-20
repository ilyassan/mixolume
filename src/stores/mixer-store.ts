import { create } from "zustand";
import {
  type AppSession,
  listSessions,
  listenToSessionsChanged,
  setVolume as setVolumeCommand,
  setMuted as setMutedCommand,
} from "@/lib/tauri";

interface MixerState {
  sessions: AppSession[];
  isLoaded: boolean;
  /** True once `init()` has registered the sessions-changed listener. */
  isInitialized: boolean;

  /** Fetches the initial session list and subscribes to backend push updates. */
  init: () => Promise<void>;
  /**
   * Optimistically updates a session's volume locally (for immediate slider
   * feedback while dragging) and asks the backend to apply it. The next
   * `sessions-changed` push will reconcile with the authoritative state.
   */
  setVolume: (sessionId: string, volume: number) => void;
  /** Optimistically toggles mute locally and asks the backend to apply it. */
  setMuted: (sessionId: string, muted: boolean) => void;
}

let unlisten: (() => void) | null = null;

export const useMixerStore = create<MixerState>((set, get) => ({
  sessions: [],
  isLoaded: false,
  isInitialized: false,

  init: async () => {
    if (get().isInitialized) {
      return;
    }
    set({ isInitialized: true });

    const stopListening = await listenToSessionsChanged((sessions) => {
      set({ sessions, isLoaded: true });
    });
    unlisten = stopListening;

    try {
      const sessions = await listSessions();
      set({ sessions, isLoaded: true });
    } catch (error) {
      console.error("Failed to load initial session list:", error);
    }
  },

  setVolume: (sessionId, volume) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === sessionId ? { ...session, volume } : session,
      ),
    }));

    setVolumeCommand(sessionId, volume).catch((error) => {
      console.error(`Failed to set volume for ${sessionId}:`, error);
    });
  },

  setMuted: (sessionId, muted) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === sessionId ? { ...session, muted } : session,
      ),
    }));

    setMutedCommand(sessionId, muted).catch((error) => {
      console.error(`Failed to set muted for ${sessionId}:`, error);
    });
  },
}));

// Exposed for tests / potential cleanup on hot-reload; not part of the
// public store API surface consumed by components.
export const __teardownMixerStoreListener = () => {
  unlisten?.();
  unlisten = null;
};
