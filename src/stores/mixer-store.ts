import { create } from "zustand";
import {
  type AppSession,
  isPermissionError,
  listSessions,
  listenToSessionsChanged,
  listenToOutputDevicesChanged,
  type OutputDevice,
  type SessionPush,
  maxVolumePercent as maxVolumePercentCommand,
  setVolume as setVolumeCommand,
  setMuted as setMutedCommand,
  setBalance as setBalanceCommand,
  outputRoutingSupported as outputRoutingSupportedCommand,
  listOutputDevices as listOutputDevicesCommand,
  setSessionOutputDevice as setSessionOutputDeviceCommand,
  duckingSupported as duckingSupportedCommand,
  getDuckingSettings as getDuckingSettingsCommand,
  setDuckingEnabled as setDuckingEnabledCommand,
  setDuckTriggerPriority as setDuckTriggerPriorityCommand,
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
    a.outputDeviceId === b.outputDeviceId &&
    iconPngEqual(a.iconPng, b.iconPng)
  );
}

/**
 * Per-session id, the `writeGeneration` (see `AppSession.writeGeneration`'s doc comment) of the
 * most recent `setVolume`/`setMuted`/`setBalance` this frontend has confirmed writing --
 * recorded the instant each of those commands *resolves*, in `setVolume`/`setMuted`/`setBalance`
 * below. `mergeSessions` uses this to reject an incoming push whose own generation is older,
 * i.e. one whose data was read by the backend *before* this write landed there.
 *
 * This exists because `draggingSessionId`'s fixed 400ms freeze window, while still a real and
 * useful defense, was confirmed live to not always be enough on its own: the backend's poll loop
 * `emit()`s a push through a JS-eval round trip into the WebView, and that call was measured to
 * occasionally block 100ms+ -- long enough, combined with however early the underlying read
 * happened relative to the write, that a push can still land *after* the freeze already released
 * and self-correct, still carrying data from before the write. A generation comparison has no
 * such timing dependency: it's correct regardless of how long any single push took to arrive.
 */
const writeGenerations = new Map<string, number>();

/** Records `generation` for `sessionId` -- but never backwards, in case two of this session's
 * own commands ever resolved out of order (the backend applies them in the order they're
 * dispatched, under one lock, but nothing guarantees their IPC responses race back in that same
 * order). */
function recordWriteGeneration(sessionId: string, generation: number): void {
  const known = writeGenerations.get(sessionId);
  if (known === undefined || generation > known) {
    writeGenerations.set(sessionId, generation);
  }
}

/**
 * Reconciles a freshly-pushed session list against what the store already has, preserving
 * object identity wherever nothing actually changed (see `sessionsEqual`'s doc comment),
 * discarding a session whose data predates this frontend's own most recent write for it (see
 * `writeGenerations`'s doc comment), and keeping the actively-dragged session's own
 * frontend-owned object entirely (see `setDraggingSessionId`'s doc comment).
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
    const knownGeneration = writeGenerations.get(incoming.id);
    if (
      previous &&
      knownGeneration !== undefined &&
      incoming.writeGeneration < knownGeneration
    ) {
      return previous;
    }
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
   * Whether the current backend can route an individual app's audio to a specific output device
   * -- currently Windows only (see `outputRoutingSupported` in lib.rs). Defaults to `false` until
   * `init()`'s fetch resolves, so the device picker stays hidden rather than flashing in.
   */
  outputRoutingSupported: boolean;
  /** Every currently available output device -- refreshed at `init()` and kept live afterward
   * via `listenToOutputDevicesChanged` (a device plugged/unplugged shows up here within its own
   * ~2s poll interval, not just at app startup). See `outputRoutingSupported`'s doc comment for
   * why this is worth fetching unconditionally rather than gating it behind a check that itself
   * needs a round trip first. */
  outputDevices: OutputDevice[];
  /** Whether auto-duck is implemented at all on this backend -- macOS and Windows, not Linux
   * (see `duckingSupported` in `tauri.ts`). Defaults to `false` until `init()`'s fetch resolves,
   * matching `outputRoutingSupported`'s own reasoning. */
  duckingSupported: boolean;
  /** True once the initial ducking-settings fetch has settled (success or failure) -- lets
   * `SettingsView` tell "still loading" apart from "loaded, and there's genuinely nothing
   * configured yet" without needing its own separate fetch. Fetched here, at `init()` time (i.e.
   * essentially at app launch), rather than lazily when Settings is first opened, specifically so
   * that by the time a user can actually click into Settings, this has almost always already
   * resolved -- see the fix this replaced for the details of the visible stutter that came from
   * only starting this fetch after `SettingsView` itself mounted (which itself only happens after
   * the page-transition's own exit animation completes), a full serial chain of delays for
   * something that has nothing to do with which page is currently showing. */
  duckingSettingsLoaded: boolean;
  duckingEnabled: boolean;
  priorityTriggers: string[];
  /** PNG icon bytes for each name in `priorityTriggers`, keyed the same way -- see the Rust
   * `DuckingSettings::priority_trigger_icons` doc comment for why this exists (keeps a real icon
   * showing in Settings even for an app that's quit or never made a sound this run). */
  priorityTriggerIcons: Record<string, number[]>;
  /**
   * The session id a slider is actively being pointer-dragged for right now, if any -- see
   * `setDraggingSessionId`'s comment for why this exists.
   */
  draggingSessionId: string | null;
  /**
   * Bumped every time `draggingSessionId` is set to a non-null value -- lets a caller that just
   * armed the freeze (`setDraggingSessionId`/`protectFromStaleEcho`) later release *only* if
   * nothing re-armed it in between, via `endFreezeIfCurrent`. See that action's own doc comment
   * for the real bug this exists to prevent.
   */
  draggingGeneration: number;

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
  /** Optimistically routes a session to `deviceId` (or back to the system default when `null`)
   * locally and asks the backend to apply it. Only meaningful when `outputRoutingSupported`. */
  setSessionOutputDevice: (sessionId: string, deviceId: string | null) => void;
  /** Optimistically toggles auto-duck locally and asks the backend to apply it, reverting on
   * failure. */
  setDuckingEnabled: (enabled: boolean) => void;
  /** Optimistically adds (`isPriority: true`) or removes (`false`) `displayName` from
   * `priorityTriggers` locally and asks the backend to apply it, reverting on failure. */
  setDuckTriggerPriority: (displayName: string, isPriority: boolean) => void;
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
  /**
   * Releases the freeze on `sessionId`, but *only* if `generation` (captured right after the
   * matching `setDraggingSessionId(sessionId)` call that armed it) still matches
   * `draggingGeneration` -- i.e. only if nothing has re-armed the freeze since.
   *
   * Both `useDraggingSessionFreeze`'s own grace-period timer and `protectFromStaleEcho` arm this
   * same single `draggingSessionId` field, and a rapid sequence of separate gestures on the same
   * session (a real click-drag pattern, not a hypothetical) can easily have one gesture's grace-
   * period timer still pending when a *later* gesture re-arms the freeze for the same session id.
   * A plain `if (draggingSessionId === sessionId)` guard (session id alone) can't tell those two
   * cases apart -- it was confirmed live, via timing diagnostics, to let a stale timer from an
   * earlier gesture release a newer gesture's freeze after only ~24ms instead of the intended
   * 400ms, which is exactly what let a stale backend push apply immediately instead of being
   * buffered, and is the root cause of the reported flicker. The generation number is the
   * actual identity a "did anything change since I armed this" check needs.
   */
  endFreezeIfCurrent: (sessionId: string, generation: number) => void;
}

// Same value as `useDraggingSessionFreeze`'s `RELEASE_GRACE_PERIOD_MS` -- see that constant's
// doc comment for why this specific duration. Not imported from there to avoid a hook-module ->
// store-module dependency; the two are independent uses of the same "give the backend's poll
// loop one full cycle" reasoning.
const STALE_ECHO_PROTECTION_MS = 400;

/**
 * Freezes `sessionId` against a stale backend echo for a moment after an optimistic write --
 * used by `setVolume`/`setBalance`/`setMuted` below, not just active pointer drags.
 *
 * Those three already had a real gap: the freeze/buffer machinery (`draggingSessionId`,
 * `pendingSessionsDuringDrag`) only ever engaged for an *actual* drag gesture, via
 * `useDraggingSessionFreeze`. A plain click (no pointer movement, just click-to-set) or a
 * keyboard change goes through these actions directly with no such protection at all -- so the
 * backend's own poll loop, running on its own independent ~150ms schedule, can have a poll
 * already in flight that read the old value *before* this write lands, and its resulting push
 * can arrive and overwrite the just-set optimistic value before a later, correct poll corrects
 * it again. Confirmed live: a plain click from 20% to 80% visibly snapping back to 20% and then
 * to 80% again, sometimes more than once. Reusing the exact same freeze this hook already relies
 * on for drags closes this the same way.
 *
 * Deliberately does nothing if a *different* session is currently drag-frozen, rather than
 * stealing that freeze -- `draggingSessionId` is a single field, and an active drag's own
 * protection matters more than this one's.
 */
function protectFromStaleEcho(sessionId: string, get: () => MixerState): void {
  if (get().draggingSessionId && get().draggingSessionId !== sessionId) {
    return;
  }
  get().setDraggingSessionId(sessionId);
  const generation = get().draggingGeneration;
  setTimeout(() => {
    get().endFreezeIfCurrent(sessionId, generation);
  }, STALE_ECHO_PROTECTION_MS);
}

let unlisten: (() => void) | null = null;
let unlistenOutputDevices: (() => void) | null = null;

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
  outputRoutingSupported: false,
  outputDevices: [],
  duckingSupported: false,
  duckingSettingsLoaded: false,
  duckingEnabled: false,
  priorityTriggers: [],
  priorityTriggerIcons: {},
  draggingSessionId: null,
  draggingGeneration: 0,

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

    outputRoutingSupportedCommand()
      .then(async (outputRoutingSupported) => {
        set({ outputRoutingSupported });
        if (!outputRoutingSupported) {
          return;
        }
        listOutputDevicesCommand()
          .then((outputDevices) => set({ outputDevices }))
          .catch((error) => {
            console.error("Failed to load output devices:", error);
          });
        // Keeps the picker's dropdown options live as devices are plugged/unplugged --
        // otherwise this list would stay frozen at whatever was connected when the app started
        // (see `listenToOutputDevicesChanged`'s own doc comment).
        unlistenOutputDevices = await listenToOutputDevicesChanged((outputDevices) => {
          set({ outputDevices });
        });
      })
      .catch((error) => {
        console.error("Failed to check output routing support:", error);
      });

    duckingSupportedCommand()
      .then((duckingSupported) => {
        set({ duckingSupported });
        if (!duckingSupported) {
          set({ duckingSettingsLoaded: true });
          return;
        }
        getDuckingSettingsCommand()
          .then((settings) => {
            set({
              duckingEnabled: settings.enabled,
              priorityTriggers: settings.priorityTriggers,
              priorityTriggerIcons: settings.priorityTriggerIcons,
              duckingSettingsLoaded: true,
            });
          })
          .catch((error) => {
            console.error("Failed to load ducking settings:", error);
            set({ duckingSettingsLoaded: true });
          });
      })
      .catch((error) => {
        console.error("Failed to check ducking support:", error);
        set({ duckingSettingsLoaded: true });
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
      sessions: state.sessions.map((session) => {
        if (session.id !== sessionId) return session;
        // Preserves whatever duck ratio was already in effect, rather than assuming the session
        // isn't currently ducked -- an outright `effectiveVolume: volume` was wrong for a ducked
        // session (it visibly showed the *full* volume, then eased back down once the next
        // backend push corrected it once the freeze below cleared -- itself a real, if smaller,
        // instance of the flicker this store's freeze/buffer machinery exists to prevent, not
        // the harmless "rare, self-correcting cosmetic blip" this comment used to claim it was).
        // `session.volume` can be 0 (muted or genuinely silent) -- the ratio is undefined then,
        // so fall back to the un-ducked case rather than dividing by zero.
        const duckRatio =
          session.isDucked && session.volume > 0 ? session.effectiveVolume / session.volume : 1;
        return { ...session, volume, effectiveVolume: volume * duckRatio };
      }),
    }));
    protectFromStaleEcho(sessionId, get);

    setVolumeCommand(sessionId, volume)
      .then((generation) => recordWriteGeneration(sessionId, generation))
      .catch((error) => {
        console.error(`Failed to set volume for ${sessionId}:`, error);
      });
  },

  setMuted: (sessionId, muted) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === sessionId ? { ...session, muted } : session,
      ),
    }));
    protectFromStaleEcho(sessionId, get);

    setMutedCommand(sessionId, muted)
      .then((generation) => recordWriteGeneration(sessionId, generation))
      .catch((error) => {
        console.error(`Failed to set muted for ${sessionId}:`, error);
      });
  },

  setBalance: (sessionId, balance) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === sessionId ? { ...session, balance } : session,
      ),
    }));
    protectFromStaleEcho(sessionId, get);

    setBalanceCommand(sessionId, balance)
      .then((generation) => recordWriteGeneration(sessionId, generation))
      .catch((error) => {
        console.error(`Failed to set balance for ${sessionId}:`, error);
      });
  },

  setSessionOutputDevice: (sessionId, deviceId) => {
    set((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === sessionId ? { ...session, outputDeviceId: deviceId } : session,
      ),
    }));
    protectFromStaleEcho(sessionId, get);

    setSessionOutputDeviceCommand(sessionId, deviceId).catch((error) => {
      console.error(`Failed to set output device for ${sessionId}:`, error);
    });
  },

  setDuckingEnabled: (enabled) => {
    const previous = get().duckingEnabled;
    set({ duckingEnabled: enabled });
    setDuckingEnabledCommand(enabled).catch((error) => {
      console.error("Failed to update auto-duck:", error);
      set({ duckingEnabled: previous });
    });
  },

  setDuckTriggerPriority: (displayName, isPriority) => {
    const previous = get().priorityTriggers;
    set({
      priorityTriggers: isPriority
        ? [...previous, displayName]
        : previous.filter((name) => name !== displayName),
    });
    setDuckTriggerPriorityCommand(displayName, isPriority).catch((error) => {
      console.error("Failed to update auto-duck trigger:", error);
      set({ priorityTriggers: previous });
    });
  },

  setDraggingSessionId: (sessionId) => {
    if (sessionId !== null) {
      set((state) => ({
        draggingSessionId: sessionId,
        draggingGeneration: state.draggingGeneration + 1,
      }));
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

  endFreezeIfCurrent: (sessionId, generation) => {
    const state = get();
    if (state.draggingSessionId !== sessionId || state.draggingGeneration !== generation) {
      return;
    }
    get().setDraggingSessionId(null);
  },
}));

// Exposed for tests / potential cleanup on hot-reload; not part of the
// public store API surface consumed by components.
export const __teardownMixerStoreListener = () => {
  unlisten?.();
  unlisten = null;
  unlistenOutputDevices?.();
  unlistenOutputDevices = null;
};

// Exposed for tests only, same reasoning as `__teardownMixerStoreListener` -- `writeGenerations`
// is module-level state outside the Zustand store itself (see its own doc comment), so
// `useMixerStore.setState(...)` alone can't reset it between test cases.
export const __resetWriteGenerationsForTests = () => {
  writeGenerations.clear();
};
