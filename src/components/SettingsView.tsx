import { useEffect, useState } from "react";
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
import {
  checkForUpdates,
  getDuckingSettings,
  setDuckingEnabled,
  setDuckTriggerPriority,
} from "@/lib/tauri";
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

export function SettingsView({ onBack }: SettingsViewProps) {
  const [openAtStartup, setOpenAtStartup] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>({
    kind: "idle",
  });
  const [duckingEnabled, setDuckingEnabledState] = useState(false);
  const [priorityApps, setPriorityApps] = useState<string[]>([]);
  const [duckingLoaded, setDuckingLoaded] = useState(false);
  const [showAddPicker, setShowAddPicker] = useState(false);
  const [pickerSearch, setPickerSearch] = useState("");
  const sessions = useMixerStore((state) => state.sessions);

  useEffect(() => {
    isAutostartEnabled()
      .then(setOpenAtStartup)
      .finally(() => setLoaded(true));
  }, []);

  useEffect(() => {
    getDuckingSettings()
      .then((settings) => {
        setDuckingEnabledState(settings.enabled);
        setPriorityApps(settings.priorityTriggers);
      })
      .finally(() => setDuckingLoaded(true));
  }, []);

  // A currently-known session's icon for a priority app, by name -- the same identity the
  // backend persists the list by (see `DuckingSettings`'s doc comment for why display name, not
  // the pid-based session id, is the stable key here). `null` (SessionIcon's generic fallback)
  // when the app isn't running right now -- the name alone is still enough to keep it in the list.
  const iconForAppName = (name: string) =>
    sessions.find((session) => session.displayName === name)?.iconPng ?? null;

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

  const toggleDucking = async () => {
    const next = !duckingEnabled;
    setDuckingEnabledState(next);
    try {
      await setDuckingEnabled(next);
    } catch (error) {
      console.error("Failed to update auto-duck:", error);
      setDuckingEnabledState(!next);
    }
  };

  const addPriorityApp = async (displayName: string) => {
    const previous = priorityApps;
    setPriorityApps([...previous, displayName]);
    setShowAddPicker(false);
    try {
      await setDuckTriggerPriority(displayName, true);
    } catch (error) {
      console.error("Failed to add auto-duck app:", error);
      setPriorityApps(previous);
    }
  };

  const removePriorityApp = async (displayName: string) => {
    const previous = priorityApps;
    setPriorityApps(previous.filter((name) => name !== displayName));
    try {
      await setDuckTriggerPriority(displayName, false);
    } catch (error) {
      console.error("Failed to remove auto-duck app:", error);
      setPriorityApps(previous);
    }
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

          <AnimatePresence initial={false}>
            {duckingEnabled && (
              <motion.div
                key="ducking-panel"
                initial={{ height: 0, opacity: 0 }}
                animate={{ height: "auto", opacity: 1 }}
                exit={{ height: 0, opacity: 0 }}
                transition={{ duration: 0.2, ease: "easeOut" }}
                className="overflow-hidden"
              >
                <div className="flex flex-col gap-1 rounded-lg bg-card/60 p-2">
                  <span className="text-muted-foreground px-1 text-xs">
                    {priorityApps.length === 0
                      ? "Add an app to get started"
                      : "Apps that trigger ducking"}
                  </span>

                  {priorityApps.length > 0 && (
                    <div className="flex max-h-40 flex-col gap-0.5 overflow-y-auto">
                      <AnimatePresence mode="popLayout" initial={false}>
                        {priorityApps.map((name) => (
                          <motion.div
                            key={name}
                            layout
                            initial={{ opacity: 0, scale: 0.96 }}
                            animate={{ opacity: 1, scale: 1 }}
                            exit={{ opacity: 0, scale: 0.96 }}
                            transition={{ duration: 0.15, ease: "easeOut" }}
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
                  )}

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
                                    layout
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
            )}
          </AnimatePresence>
        </div>

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
