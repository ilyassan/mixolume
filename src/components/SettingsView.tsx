import { useEffect, useState } from "react";
import { ArrowLeft } from "lucide-react";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";
import { Button } from "@/components/ui/button";
import { Wordmark } from "@/components/Wordmark";
import {
  checkForUpdates,
  getDuckingSettings,
  setDuckingEnabled,
  setDuckTriggerExcluded,
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
  const [excludedTriggers, setExcludedTriggers] = useState<string[]>([]);
  const [duckingLoaded, setDuckingLoaded] = useState(false);
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
        setExcludedTriggers(settings.excludedTriggers);
      })
      .finally(() => setDuckingLoaded(true));
  }, []);

  // Every app MiXolume currently knows about, deduplicated by name -- the same identity the
  // backend persists exclusions by (see `DuckingSettings`'s doc comment for why display name,
  // not the pid-based session id, is the stable key here).
  const knownAppNames = Array.from(
    new Set(sessions.map((session) => session.displayName)),
  ).sort((a, b) => a.localeCompare(b));

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

  const toggleAppWatched = async (displayName: string, watched: boolean) => {
    // "Watched" in the UI means "not excluded" -- the backend only stores exclusions, so a
    // checked box (watched = true) means we want it removed from that list.
    const nextExcluded = watched
      ? excludedTriggers.filter((name) => name !== displayName)
      : [...excludedTriggers, displayName];
    setExcludedTriggers(nextExcluded);
    try {
      await setDuckTriggerExcluded(displayName, !watched);
    } catch (error) {
      console.error("Failed to update auto-duck app list:", error);
      setExcludedTriggers(excludedTriggers);
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
                Lowers everything else while an app plays speech — a call, a
                voice message, dialogue in a video.
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

          {duckingEnabled && knownAppNames.length > 0 && (
            <div className="flex flex-col gap-1 rounded-lg bg-card/60 p-2">
              <span className="text-muted-foreground px-1 text-xs">
                Apps that can trigger ducking
              </span>
              {knownAppNames.map((name) => {
                const watched = !excludedTriggers.includes(name);
                return (
                  <label
                    key={name}
                    className="flex items-center gap-2 rounded px-1 py-0.5 text-sm"
                  >
                    <input
                      type="checkbox"
                      checked={watched}
                      onChange={(e) => toggleAppWatched(name, e.target.checked)}
                      className="size-3.5 accent-primary"
                    />
                    <span className="truncate">{name}</span>
                  </label>
                );
              })}
            </div>
          )}
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
        {updateStatus.kind === "up-to-date" && (
          <p className="text-muted-foreground text-xs">
            You're on the latest version.
          </p>
        )}
        {updateStatus.kind === "installed" && (
          <p className="text-xs text-primary">
            Version {updateStatus.version} downloaded — restart MiXolume to
            finish updating.
          </p>
        )}
        {updateStatus.kind === "error" && (
          <p className="text-muted-foreground text-xs">
            Couldn't check for updates. Try again later.
          </p>
        )}
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
