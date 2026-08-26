import { create } from "zustand";
import {
  type AppSession,
  isPermissionError,
  listSessions,
  listenToSessionsChanged,
  setVolume as setVolumeCommand,
  setMuted as setMutedCommand,
  setBalance as setBalanceCommand,
} from "@/lib/tauri";

interface MixerState {
  sessions: AppSession[];
  isLoaded: boolean;
  /** True once `init()` has registered the sessions-changed listener. */
  isInitialized: boolean;
  /**
   * True while the backend is reporting it's still waiting on Screen &
   * System Audio Recording permission. Kept up to date by an ongoing retry
   * loop rather than latched permanently -- whether a mid-session grant
   * takes effect without an app relaunch has been inconsistent in practice,
   * so the UI shouldn't assume either way and should just reflect whatever
   * the backend reports on the next check.
   */
  needsPermission: boolean;

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
  /** Optimistically updates left/right balance locally and asks the backend to apply it. */
  setBalance: (sessionId: string, balance: number) => void;
}

let unlisten: (() => void) | null = null;

export const useMixerStore = create<MixerState>((set, get) => ({
  sessions: [],
  isLoaded: false,
  isInitialized: false,
  needsPermission: false,

  init: async () => {
    if (get().isInitialized) {
      return;
    }
    set({ isInitialized: true });

    const stopListening = await listenToSessionsChanged((sessions) => {
      set({ sessions, isLoaded: true, needsPermission: false });
    });
    unlisten = stopListening;

    const tryLoad = async (): Promise<boolean> => {
      if (get().isLoaded) {
        return true;
      }
      try {
        const sessions = await listSessions();
        set({ sessions, isLoaded: true, needsPermission: false });
        return true;
      } catch (error) {
        console.error("Failed to load initial session list:", error);
        set({ needsPermission: isPermissionError(error) });
        return false;
      }
    };

    if (await tryLoad()) {
      return;
    }

    // Keep retrying on an interval rather than giving up after one failure.
    // Two distinct failure modes land here, and both call for retrying
    // rather than a one-shot check: (1) the very first `listSessions()`
    // call right after window creation can occasionally lose a race in the
    // native IPC bridge's own startup, with no error thrown -- it just never
    // resolves with real data, and nothing else self-corrects since the
    // backend's push loop only emits when the session list *changes* from
    // what it last sent; (2) a permission-wait error, where whether a grant
    // made while the app is already running takes effect without a full
    // relaunch has been inconsistent in practice -- rather than assume it
    // never will and get stuck showing "permission needed" forever even
    // after the user has actually granted it, keep checking and let
    // `needsPermission` clear itself the moment a check actually succeeds.
    const retryInterval = setInterval(() => {
      void tryLoad().then((succeeded) => {
        if (succeeded) {
          clearInterval(retryInterval);
        }
      });
    }, 2000);
  },

  setVolume: (sessionId, volume) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        // Optimistically assumes not currently ducked (the common case) so a normal drag feels
        // immediate -- if the session actually is mid-duck, the next backend push (within
        // ~700ms) corrects `effectiveVolume` back down. A duck happening to start/end in that
        // exact window is a rare, self-correcting cosmetic blip, not a real bug.
        session.id === sessionId
          ? { ...session, volume, effectiveVolume: volume }
          : session,
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

  setBalance: (sessionId, balance) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === sessionId ? { ...session, balance } : session,
      ),
    }));

    setBalanceCommand(sessionId, balance).catch((error) => {
      console.error(`Failed to set balance for ${sessionId}:`, error);
    });
  },
}));

// Exposed for tests / potential cleanup on hot-reload; not part of the
// public store API surface consumed by components.
export const __teardownMixerStoreListener = () => {
  unlisten?.();
  unlisten = null;
};
