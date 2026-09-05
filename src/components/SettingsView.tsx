import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ArrowLeft, Plus, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";
import { Button } from "@/components/ui/button";
import { Wordmark } from "@/components/Wordmark";
import { SessionIcon } from "@/components/SessionIcon";
import { checkForUpdates } from "@/lib/tauri";
import { useMixerStore } from "@/stores/mixer-store";
import icon from "@/assets/icon.svg";
import pkg from "../../package.json";

interface SettingsViewProps {
  onBack: () => void;
}

type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "up-to-date" }
  | { kind: "installed"; version: string }
  | { kind: "error" };

interface SessionIdentity {
  id: string;
  displayName: string;
  iconPng: number[] | null;
}

/** All that this view ever actually reads off a session (name + icon, for the priority-app rows
 * and the "add app" picker) -- deliberately not `volume`/`effectiveVolume`/`isActive`/duck flags,
 * which change on essentially every ~150ms backend poll tick regardless of whether anything this
 * view displays actually changed. Used as the equality check for the `sessions` selector below;
 * `iconPng` compares by reference (not byte-for-byte), matching `mixer-store.ts`'s own
 * `resolvePushedIcons`, which deliberately carries the *same* array reference forward across polls
 * when an icon hasn't changed -- so a real icon change is still caught, without a byte compare. */
function sessionIdentitiesEqual(
  a: SessionIdentity[],
  b: SessionIdentity[],
): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (
      a[i].id !== b[i].id ||
      a[i].displayName !== b[i].displayName ||
      a[i].iconPng !== b[i].iconPng
    ) {
      return false;
    }
  }
  return true;
}

export function SettingsView({ onBack }: SettingsViewProps) {
  const [openAtStartup, setOpenAtStartup] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>({
    kind: "idle",
  });
  const [showAddPicker, setShowAddPicker] = useState(false);
  const [pickerSearch, setPickerSearch] = useState("");
  // Deliberately not `useMixerStore((state) => state.sessions)` -- the raw array gets a fresh
  // reference on essentially every ~150ms backend poll tick (volume/active-state churn, mostly
  // unrelated to anything this view shows), which reactively re-rendered this whole component
  // that often for as long as Settings stayed open. Confirmed live as the actual cause of the
  // ducking panel's toggle animation feeling smooth sometimes and instantly-snapped other times:
  // purely down to whether a poll-driven re-render happened to land in the middle of Framer
  // Motion's `height: auto` measurement at the exact moment the user clicked, a race with no
  // relationship to anything the user was actually doing. The custom equality check here only
  // treats this as "changed" when a session's identity (name/icon) actually changed, not its
  // volume or active state -- see `sessionIdentitiesEqual`'s own doc comment.
  // `useSyncExternalStore` directly, not a plain `useMixerStore(selector)` call -- Zustand v5's
  // default hook dropped the second `equalityFn` argument the classic API had, so a custom
  // comparison needs its own escape hatch. `getSnapshot` below caches its last result in a ref and
  // only replaces it when `sessionIdentitiesEqual` says something actually changed, so React's own
  // `Object.is` check (which is what decides whether to re-render) sees a stable reference across
  // polls that didn't change anything this view cares about.
  const sessionsCacheRef = useRef<SessionIdentity[]>([]);
  const sessions = useSyncExternalStore(useMixerStore.subscribe, () => {
    const next = useMixerStore.getState().sessions.map((session) => ({
      id: session.id,
      displayName: session.displayName,
      iconPng: session.iconPng,
    }));
    if (!sessionIdentitiesEqual(sessionsCacheRef.current, next)) {
      sessionsCacheRef.current = next;
    }
    return sessionsCacheRef.current;
  });
  // Ducking settings are fetched once in `mixer-store.ts`'s `init()` -- essentially at app
  // launch, well before the user can click into Settings at all -- rather than lazily here on
  // mount. This component used to fetch them itself, in a `useEffect` that only ever fires after
  // this view actually mounts, which itself only happens after the page-transition's own exit
  // animation finishes -- confirmed live as a real, visible stutter: a whole serial chain of
  // delays (transition, then mount, then paint, then only *then* start fetching) for data that
  // has nothing to do with which page happens to be showing. Reading it from the store instead
  // means it's very often already loaded by the time this view exists, and the fetch itself only
  // ever happens once no matter how many times Settings is opened and closed.
  const duckingIsSupported = useMixerStore((state) => state.duckingSupported);
  const duckingLoaded = useMixerStore((state) => state.duckingSettingsLoaded);
  const duckingEnabled = useMixerStore((state) => state.duckingEnabled);
  const priorityApps = useMixerStore((state) => state.priorityTriggers);
  const priorityAppIcons = useMixerStore((state) => state.priorityTriggerIcons);
  const setDuckingEnabledStore = useMixerStore((state) => state.setDuckingEnabled);
  const setDuckTriggerPriorityStore = useMixerStore(
    (state) => state.setDuckTriggerPriority,
  );

  useEffect(() => {
    isAutostartEnabled()
      .then(setOpenAtStartup)
      .finally(() => setLoaded(true));
  }, []);

  // A priority app's icon, by name -- the same identity the backend persists the list by (see
  // `DuckingSettings`'s doc comment for why display name, not the pid-based session id, is the
  // stable key here). Prefers a currently-live session's icon (freshest, and covers an app added
  // before this cache existed); falls back to the backend's persisted `priorityTriggerIcons` when
  // the app isn't currently running. `null` (SessionIcon's generic fallback) only when neither is
  // available yet -- the name alone is still enough to keep it in the list either way.
  const iconForAppName = (name: string) =>
    sessions.find((session) => session.displayName === name)?.iconPng ??
    priorityAppIcons[name] ??
    null;

  // Apps MiXolume has actually seen making sound (active or not) and isn't already tracking as a
  // priority app, filtered by the search box. Deliberately scoped to sessions rather than every
  // running app on the Mac -- an earlier version searched all running apps via `NSWorkspace`, but
  // resolving icons for many apps at once turned out to be inherently slow (real per-icon AppKit
  // decode cost, not something caching alone fixed) and caused a multi-second/-minute Settings
  // freeze. Common call apps the user hasn't opened yet are covered separately by the default
  // seeding in `set_ducking_enabled` (macos.rs) when auto-duck is first turned on.
  const searchQuery = pickerSearch.trim().toLowerCase();
  const addableApps = Array.from(
    new Map(
      sessions
        .filter((session) => !priorityApps.includes(session.displayName))
        .map((session) => [session.displayName, session] as const),
    ).values(),
  )
    .filter((session) =>
      session.displayName.toLowerCase().includes(searchQuery),
    )
    .sort((a, b) => a.displayName.localeCompare(b.displayName));

  const toggleOpenAtStartup = async () => {
    const next = !openAtStartup;
    // Optimistic update, like the volume/mute controls elsewhere in the app -- reverted below
    // if the underlying call actually fails (e.g. sandboxing denies the startup-item write).
    setOpenAtStartup(next);
    try {
      if (next) {
        await enableAutostart();
      } else {
        await disableAutostart();
      }
    } catch (error) {
      console.error("Failed to update open-at-startup:", error);
      setOpenAtStartup(!next);
    }
  };

  const toggleDucking = () => {
    setDuckingEnabledStore(!duckingEnabled);
  };

  const addPriorityApp = (displayName: string) => {
    setDuckTriggerPriorityStore(displayName, true);
    setShowAddPicker(false);
  };

  const removePriorityApp = (displayName: string) => {
    setDuckTriggerPriorityStore(displayName, false);
  };

  const handleCheckForUpdates = async () => {
    setUpdateStatus({ kind: "checking" });
    try {
      const outcome = await checkForUpdates();
      setUpdateStatus(
        outcome.status === "installed"
          ? { kind: "installed", version: outcome.version }
          : { kind: "up-to-date" },
      );
    } catch (error) {
      console.error("Failed to check for updates:", error);
      setUpdateStatus({ kind: "error" });
    }
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-1 border-b border-border px-2 py-2">
        <Button variant="ghost" size="icon" onClick={onBack} aria-label="Back">
          <ArrowLeft className="size-4" />
        </Button>
        <span className="text-sm font-medium">Settings</span>
      </div>

      <div className="flex flex-col gap-4 p-4">
        <label className="flex items-center justify-between gap-3">
          <span className="text-sm">Open at startup</span>
          <button
            type="button"
            role="switch"
            aria-checked={openAtStartup}
            disabled={!loaded}
            onClick={toggleOpenAtStartup}
            className={`relative h-5 w-9 shrink-0 rounded-full transition-colors disabled:opacity-50 ${
              openAtStartup ? "bg-primary" : "bg-input"
            }`}
          >
            <span
              className={`absolute top-0.5 left-0.5 size-4 rounded-full bg-white shadow transition-transform ${
                openAtStartup ? "translate-x-4" : "translate-x-0"
              }`}
            />
          </button>
        </label>

        {duckingIsSupported && (
        <div className="flex flex-col gap-2">
          <label className="flex items-center justify-between gap-3">
            <span className="flex flex-col">
              <span className="text-sm">Auto-duck other apps</span>
              <span className="text-muted-foreground text-xs">
                Add apps you take calls or voice messages in. When one of
                them is talking, everything else quiets down, then comes back
                up when it's done.
              </span>
            </span>
            <button
              type="button"
              role="switch"
              aria-checked={duckingEnabled}
              disabled={!duckingLoaded}
              onClick={toggleDucking}
              className={`relative h-5 w-9 shrink-0 rounded-full transition-colors disabled:opacity-50 ${
                duckingEnabled ? "bg-primary" : "bg-input"
              }`}
            >
              <span
                className={`absolute top-0.5 left-0.5 size-4 rounded-full bg-white shadow transition-transform ${
                  duckingEnabled ? "translate-x-4" : "translate-x-0"
                }`}
              />
            </button>
          </label>

          {/* Always mounted (no `AnimatePresence`/conditional-render around this `motion.div`
              itself) -- only its `height`/`opacity` animate based on `duckingEnabled`. This used
              to be `{duckingEnabled && (<motion.div key="ducking-panel" .../>)}`, which mounted
              and unmounted this entire subtree -- rows, icons, everything -- on every single
              toggle. Confirmed live as a real, perceptible stutter distinct from the "Open at
              startup" toggle right above it (a trivial local boolean with no associated content):
              every toggle re-ran every row's own `SessionIcon`/`useIconObjectUrl` effect from
              scratch (a fresh `Blob`+`URL.createObjectURL` per icon, not free), on top of Framer
              Motion re-registering every row's `layout="position"` tracking and re-measuring the
              panel's own `height: auto` target, all synchronously around the same click. Keeping
              this mounted permanently means icons decode once and stay decoded regardless of how
              many times the switch gets flipped -- toggling becomes a plain height/opacity
              transition with nothing underneath it to rebuild. */}
          <motion.div
            // Guarantees no animation plays on `SettingsView`'s own mount (only on a genuine
            // later toggle) -- without this, whatever `duckingEnabled` happens to already be by
            // the time this first renders (now often `true` already, since ducking settings are
            // prefetched at app launch) would otherwise animate in from the opposite state.
            initial={false}
            animate={{
              height: duckingEnabled ? "auto" : 0,
              opacity: duckingEnabled ? 1 : 0,
            }}
            transition={{ duration: 0.2, ease: "easeOut" }}
            className="overflow-hidden"
            aria-hidden={!duckingEnabled}
          >
                <div className="flex flex-col gap-1 rounded-lg bg-card/60 p-2">
                  {/* `duckingLoaded` gates this rather than `priorityApps.length` alone -- before
                      the initial `getDuckingSettings()` round trip resolves, an empty array just
                      means "don't know yet," not "there really are zero apps," so showing "Add an
                      app to get started" during that brief window would be actively misleading,
                      not just a cosmetic flash.
                      No `initial={false}` here (unlike page-level transitions elsewhere in this
                      app) -- that prop only suppresses the animation of whatever's already present
                      the *very first time* this `AnimatePresence` commits, and a fast-enough local
                      Tauri round trip can resolve before that first paint even happens, which would
                      make this text count as "already there" and skip its fade-in entirely instead
                      of easing in the way it's supposed to. This text should always ease in on its
                      first real appearance, no matter how quickly the fetch happens to resolve. */}
                  {/* Both this text and the row list below delay their *entrance* specifically
                      (a per-keyframe `transition` on `animate`, not the shared one exit already
                      uses) until just after the panel's own `height: 0 -> auto` reveal above has
                      finished. The rows live inside that panel's `overflow-hidden`, so without
                      this, their own fade-in races the exact same duration as the reveal that's
                      clipping them -- confirmed to read as "the panel opens on an empty list, then
                      it's suddenly full" rather than one clean sequence, since a row can finish
                      fading to opaque before the panel has grown enough to actually show it. */}
                  <AnimatePresence mode="wait">
                    {duckingLoaded && (
                      <motion.span
                        key={priorityApps.length === 0 ? "empty" : "populated"}
                        initial={{ opacity: 0 }}
                        animate={{
                          opacity: 1,
                          transition: { duration: 0.15, ease: "easeOut", delay: 0.2 },
                        }}
                        exit={{ opacity: 0, transition: { duration: 0.15, ease: "easeOut" } }}
                        className="text-muted-foreground px-1 text-xs"
                      >
                        {priorityApps.length === 0
                          ? "Add an app to get started"
                          : "Apps that trigger ducking"}
                      </motion.span>
                    )}
                  </AnimatePresence>

                  {/* Stays mounted regardless of `priorityApps.length` -- conditionally mounting
                      this whole block on `priorityApps.length > 0` instead (as a naive read of
                      "only show the list when there's something to show" suggests) would make
                      `AnimatePresence` first commit exactly when the loaded apps appear, which
                      matters because of the *other* mistake this fix corrects: no `initial={false}`
                      below either, for the same reason as the header text's `AnimatePresence` above
                      -- a fast-enough local Tauri round trip can resolve before this component's
                      first paint, and `initial={false}` would then treat that very first batch of
                      rows as "already there" and skip their entrance animation entirely, which is
                      the exact instant "boom" this whole fix exists to prevent. */}
                  <div className="flex max-h-40 flex-col gap-0.5 overflow-y-auto">
                    <AnimatePresence mode="popLayout">
                      {duckingLoaded &&
                        priorityApps.map((name) => (
                          <motion.div
                            key={name}
                            // `layout="position"`, not the plain `layout` boolean (which also
                            // tracks size, not just position) -- this list lives inside the
                            // ducking panel's own `height: "auto"` reveal above, and Framer Motion
                            // already has to force real layout measurement to animate that.
                            // Stacking a full-size-tracking `layout` animation for every row on
                            // top of that, at the same time, was confirmed to produce a real,
                            // visible main-thread freeze (layout thrashing: measure, animate,
                            // re-measure, repeated per row, per frame) -- not just a hypothetical
                            // cost. Position-only tracking (a `SessionRow.tsx` precedent, see its
                            // own comment) still animates reordering smoothly without forcing that
                            // extra size remeasurement.
                            layout="position"
                            initial={{ opacity: 0, scale: 0.96 }}
                            animate={{
                              opacity: 1,
                              scale: 1,
                              transition: { duration: 0.15, ease: "easeOut", delay: 0.2 },
                            }}
                            exit={{
                              opacity: 0,
                              scale: 0.96,
                              transition: { duration: 0.15, ease: "easeOut" },
                            }}
                            className="flex items-center gap-2 rounded px-1 py-1"
                          >
                            <SessionIcon
                              iconPng={iconForAppName(name)}
                              displayName={name}
                            />
                            <span className="min-w-0 flex-1 truncate text-sm">
                              {name}
                            </span>
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon"
                              className="text-muted-foreground size-6"
                              aria-label={`Remove ${name}`}
                              onClick={() => removePriorityApp(name)}
                            >
                              <X className="size-3.5" />
                            </Button>
                          </motion.div>
                        ))}
                    </AnimatePresence>
                  </div>

                  <AnimatePresence initial={false}>
                    {showAddPicker && (
                      <motion.div
                        key="add-picker"
                        initial={{ height: 0, opacity: 0 }}
                        animate={{ height: "auto", opacity: 1 }}
                        exit={{ height: 0, opacity: 0 }}
                        transition={{ duration: 0.18, ease: "easeOut" }}
                        className="overflow-hidden"
                      >
                        <div className="flex flex-col gap-1 border-t border-border pt-1">
                          <input
                            type="text"
                            autoFocus
                            value={pickerSearch}
                            onChange={(event) =>
                              setPickerSearch(event.target.value)
                            }
                            placeholder="Search running apps…"
                            className="border-border bg-background placeholder:text-muted-foreground rounded border px-2 py-1 text-sm outline-none focus:border-primary"
                          />
                          {addableApps.length > 0 ? (
                            <div className="flex max-h-40 flex-col gap-0.5 overflow-y-auto">
                              <AnimatePresence mode="popLayout" initial={false}>
                                {addableApps.map((session) => (
                                  <motion.button
                                    key={session.displayName}
                                    // Same reasoning as the priority-apps list above: this sits
                                    // inside the "add-picker" panel's own `height: "auto"` reveal,
                                    // so a full-size-tracking `layout` here stacks another forced
                                    // layout measurement on top of that at the same time.
                                    layout="position"
                                    initial={{ opacity: 0 }}
                                    animate={{ opacity: 1 }}
                                    exit={{ opacity: 0 }}
                                    transition={{
                                      duration: 0.12,
                                      ease: "easeOut",
                                    }}
                                    type="button"
                                    onClick={() =>
                                      addPriorityApp(session.displayName)
                                    }
                                    className="hover:bg-accent flex items-center gap-2 rounded px-1 py-1 text-left"
                                  >
                                    <SessionIcon
                                      iconPng={session.iconPng}
                                      displayName={session.displayName}
                                    />
                                    <span className="min-w-0 flex-1 truncate text-sm">
                                      {session.displayName}
                                    </span>
                                  </motion.button>
                                ))}
                              </AnimatePresence>
                            </div>
                          ) : (
                            <p className="text-muted-foreground px-1 py-1 text-xs">
                              {searchQuery
                                ? "No matching apps."
                                : "No other apps are currently playing audio to add."}
                            </p>
                          )}
                        </div>
                      </motion.div>
                    )}
                  </AnimatePresence>

                  <Button
                    type="button"
                    variant="outline"
                    className="mt-1 h-7 justify-start gap-1.5 px-2 text-xs"
                    onClick={() => {
                      const opening = !showAddPicker;
                      setShowAddPicker(opening);
                      if (opening) {
                        setPickerSearch("");
                      }
                    }}
                  >
                    <AnimatePresence mode="popLayout" initial={false}>
                      {showAddPicker ? (
                        <motion.span
                          key="close"
                          initial={{ opacity: 0, rotate: -45 }}
                          animate={{ opacity: 1, rotate: 0 }}
                          exit={{ opacity: 0, rotate: 45 }}
                          transition={{ duration: 0.12, ease: "easeOut" }}
                          className="inline-flex"
                        >
                          <X className="size-3.5" />
                        </motion.span>
                      ) : (
                        <motion.span
                          key="plus"
                          initial={{ opacity: 0, rotate: -45 }}
                          animate={{ opacity: 1, rotate: 0 }}
                          exit={{ opacity: 0, rotate: 45 }}
                          transition={{ duration: 0.12, ease: "easeOut" }}
                          className="inline-flex"
                        >
                          <Plus className="size-3.5" />
                        </motion.span>
                      )}
                    </AnimatePresence>
                    {showAddPicker ? "Done" : "Add app"}
                  </Button>
                </div>
          </motion.div>
        </div>
        )}

        <div className="flex items-center justify-between gap-3">
          <span className="text-sm">Updates</span>
          <Button
            variant="outline"
            className="h-7 px-2.5 text-xs"
            disabled={updateStatus.kind === "checking"}
            onClick={handleCheckForUpdates}
          >
            {updateStatus.kind === "checking"
              ? "Checking…"
              : "Check for Updates"}
          </Button>
        </div>
        <AnimatePresence mode="wait" initial={false}>
          {updateStatus.kind === "up-to-date" && (
            <motion.p
              key="up-to-date"
              initial={{ opacity: 0, y: -4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -4 }}
              transition={{ duration: 0.15, ease: "easeOut" }}
              className="text-muted-foreground text-xs"
            >
              You're on the latest version.
            </motion.p>
          )}
          {updateStatus.kind === "installed" && (
            <motion.p
              key="installed"
              initial={{ opacity: 0, y: -4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -4 }}
              transition={{ duration: 0.15, ease: "easeOut" }}
              className="text-xs text-primary"
            >
              Version {updateStatus.version} downloaded — restart MiXolume to
              finish updating.
            </motion.p>
          )}
          {updateStatus.kind === "error" && (
            <motion.p
              key="error"
              initial={{ opacity: 0, y: -4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -4 }}
              transition={{ duration: 0.15, ease: "easeOut" }}
              className="text-muted-foreground text-xs"
            >
              Couldn't check for updates. Try again later.
            </motion.p>
          )}
        </AnimatePresence>
      </div>

      <div className="mt-auto flex flex-col items-center gap-1 border-t border-border p-4 text-center">
        <img src={icon} alt="" className="size-8 rounded-[8px]" />
        <Wordmark className="text-sm" />
        <span className="text-muted-foreground text-xs">
          Version {pkg.version}
        </span>
      </div>
    </div>
  );
}
