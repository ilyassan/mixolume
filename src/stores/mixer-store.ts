import { create } from "zustand";
import {
  type AppSession,
  isPermissionError,
  listSessions,
  listenToSessionsChanged,
  type SessionPush,
  maxVolumePercent as maxVolumePercentCommand,
  setVolume as setVolumeCommand,
  setMuted as setMutedCommand,
  setBalance as setBalanceCommand,
} from "@/lib/tauri";

function iconPngEqual(a: number[] | null, b: number[] | null): boolean {
  if (a === b) return true;
  if (a === null || b === null || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/**
 * Turns a `sessions-changed` push into full `AppSession`s by filling in every icon the backend
 * left out because it had already sent it (see `SessionPush`). `previousIcons` is whatever the
 * frontend currently holds for each id -- carrying the *same array reference* forward, so the
 * identity checks in `sessionsEqual`/`useIconObjectUrl` still short-circuit instead of walking
 * two byte arrays or re-decoding a `Blob`.
 */
function resolvePushedIcons(
  incomingSessions: SessionPush[],
  previousIcons: Map<string, number[] | null>,
): AppSession[] {
  return incomingSessions.map(({ iconPng, ...rest }) => ({
    ...rest,
    iconPng: iconPng === undefined ? (previousIcons.get(rest.id) ?? null) : iconPng,
  }));
}

function iconsById(
  sessions: readonly AppSession[],
): Map<string, number[] | null> {
  return new Map(sessions.map((session) => [session.id, session.iconPng]));
}

/** Value equality for a freshly-pushed session against the previous one with the same id. */
function sessionsEqual(a: AppSession, b: AppSession): boolean {
  return (
    a.displayName === b.displayName &&
    a.volume === b.volume &&
    a.effectiveVolume === b.effectiveVolume &&
    a.muted === b.muted &&
    a.balance === b.balance &&
    a.isActive === b.isActive &&
    a.isDuckTrigger === b.isDuckTrigger &&
    a.isDucked === b.isDucked &&
    iconPngEqual(a.iconPng, b.iconPng)
  );
}

/**
 * Reconciles a freshly-pushed session list against what the store already has, preserving
 * object identity wherever nothing actually changed (see `sessionsEqual`'s doc comment) and
 * keeping the actively-dragged session's own frontend-owned object entirely (see
 * `setDraggingSessionId`'s doc comment).
 */
function mergeSessions(
  previousSessions: AppSession[],
  incomingSessions: AppSession[],
  draggingSessionId: string | null,
): AppSession[] {
  const previousById = new Map(previousSessions.map((s) => [s.id, s]));
  return incomingSessions.map((incoming) => {
    if (incoming.id === draggingSessionId) {
      const dragged = previousById.get(incoming.id);
      if (dragged) return dragged;
    }
    const previous = previousById.get(incoming.id);
    if (previous && sessionsEqual(previous, incoming)) {
      return previous;
    }
    return incoming;
  });
}

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
  /**
   * Highest volume percent the current backend allows (100 normally, 200 on macOS's boosted
   * backend -- see `max_volume_percent` in lib.rs). Defaults to 100 until `init()`'s fetch
   * resolves, so a slow/failed fetch degrades to today's unboosted behavior rather than an
   * inconsistent or broken slider.
   */
  maxVolumePercent: number;
  /**
   * The session id a slider is actively being pointer-dragged for right now, if any -- see
   * `setDraggingSessionId`'s comment for why this exists.
   */
  draggingSessionId: string | null;

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
  /**
   * Called by `useDraggingSessionFreeze` (used from both `VolumeSlider` and `BalanceSliders`)
   * around a drag gesture, plus a short grace period after it ends -- see that hook's own doc
   * comment for why the grace period matters. While a session id is set here, an incoming backend
   * push (see `init`'s `listenToSessionsChanged` handler) keeps that one session's *existing*
   * object reference instead of replacing it with the freshly-pushed one.
   *
   * Why this matters: those two components call the backend directly (throttled) during a drag
   * rather than going through `setVolume`/`setBalance`, specifically so this row doesn't get a new
   * `session` prop -- and therefore doesn't re-render, and therefore doesn't force its `motion.div`
   * wrapper through Framer Motion's layout-remeasurement -- on every drag tick. But the backend's
   * own poll loop keeps running independently on its own ~150ms schedule the whole time, and since
   * a drag is genuinely changing that session's real volume, its "only push if something changed"
   * gate has no reason to suppress those pushes -- so without this, the row would still get a new
   * `session` reference (and pay the same re-render+layout cost) roughly every poll tick, on a
   * timer unrelated to how fast the pointer is actually moving. Confirmed by isolating a slider
   * with zero backend/store involvement at all, which felt perfectly smooth where the real ones
   * didn't -- the gap was this recurring push, not anything in the drag-tick path itself.
   */
  setDraggingSessionId: (sessionId: string | null) => void;
}

let unlisten: (() => void) | null = null;

/**
 * The most recent backend push received while a drag was in progress, held here (not in React
 * state) so receiving it doesn't itself cause a re-render -- applied once the drag actually ends,
 * in `setDraggingSessionId`. `null` means no push has arrived since the current drag (if any)
 * started, or since it last ended.
 */
let pendingSessionsDuringDrag: AppSession[] | null = null;

export const useMixerStore = create<MixerState>((set, get) => ({
  sessions: [],
  isLoaded: false,
  isInitialized: false,
  needsPermission: false,
  maxVolumePercent: 100,
  draggingSessionId: null,

  init: async () => {
    if (get().isInitialized) {
      return;
    }
    set({ isInitialized: true });

    maxVolumePercentCommand()
      .then((maxVolumePercent) => set({ maxVolumePercent }))
      .catch((error) => {
        console.error("Failed to load max volume percent:", error);
      });

    const stopListening = await listenToSessionsChanged((pushed) => {
      if (get().draggingSessionId) {
        // Resolved against the *buffered* push when there is one, not just the store: only the
        // most recent push survives a drag, so an icon that arrived (and was therefore omitted
        // from every push after it) mid-drag would otherwise be dropped for good.
        const sessions = resolvePushedIcons(
          pushed,
          iconsById(pendingSessionsDuringDrag ?? get().sessions),
        );
        // Buffer instead of touching React state at all -- see `pendingSessionsDuringDrag`'s
        // and `setDraggingSessionId`'s doc comments for why *any* store update during a drag,
        // even one that (via `mergeSessions`) ends up a no-op for the dragged row itself, still
        // has a real cost: the outer `sessions` array is a fresh reference every push regardless,
        // which re-renders whatever maps over it and re-registers every row with Framer Motion's
        // shared layout-projection system for that frame. Confirmed live as still glitchy even
        // after that projection system was suspended for just the dragged row's own re-renders --
        // the other rows' pushes were still reaching it. Not applying *anything* to the store
        // until the drag ends removes this entirely, matching how a native (non-webview) app in
        // the same problem space does it: its own volume changes mutate the UI's backing state
        // directly and instantly, with no round-trip involved at all.
        pendingSessionsDuringDrag = sessions;
        return;
      }
      set((state) => ({
        sessions: mergeSessions(
          state.sessions,
          resolvePushedIcons(pushed, iconsById(state.sessions)),
          state.draggingSessionId,
        ),
        isLoaded: true,
        needsPermission: false,
      }));
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

  setDraggingSessionId: (sessionId) => {
    if (sessionId !== null) {
      set({ draggingSessionId: sessionId });
      return;
    }
    // Drag (plus its grace period, see `useDraggingSessionFreeze`) has ended -- apply whatever
    // the backend pushed most recently while it was buffered (see `pendingSessionsDuringDrag`'s
    // doc comment), if anything arrived, in the same update that clears the freeze. `sessionId`
    // is already `null` here, so `mergeSessions` no longer special-cases any row -- correct,
    // since nothing is actively dragging anymore by this point.
    const pending = pendingSessionsDuringDrag;
    pendingSessionsDuringDrag = null;
    if (pending) {
      set((state) => ({
        sessions: mergeSessions(state.sessions, pending, null),
        draggingSessionId: null,
      }));
    } else {
      set({ draggingSessionId: null });
    }
  },
}));

// Exposed for tests / potential cleanup on hot-reload; not part of the
// public store API surface consumed by components.
export const __teardownMixerStoreListener = () => {
  unlisten?.();
  unlisten = null;
};
