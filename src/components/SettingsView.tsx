import { useEffect, useState } from "react";
import { ArrowLeft } from "lucide-react";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";
import { Button } from "@/components/ui/button";
import { Wordmark } from "@/components/Wordmark";
import { checkForUpdates } from "@/lib/tauri";
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

  useEffect(() => {
    isAutostartEnabled()
      .then(setOpenAtStartup)
      .finally(() => setLoaded(true));
  }, []);

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
