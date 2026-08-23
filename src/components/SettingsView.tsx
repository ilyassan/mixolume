import { useEffect, useState } from "react";
import { ArrowLeft } from "lucide-react";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";
import { Button } from "@/components/ui/button";
import { Wordmark } from "@/components/Wordmark";
import logo from "@/assets/logo.png";
import pkg from "../../package.json";

interface SettingsViewProps {
  onBack: () => void;
}

export function SettingsView({ onBack }: SettingsViewProps) {
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    isAutostartEnabled()
      .then(setLaunchAtLogin)
      .finally(() => setLoaded(true));
  }, []);

  const toggleLaunchAtLogin = async () => {
    const next = !launchAtLogin;
    // Optimistic update, like the volume/mute controls elsewhere in the app -- reverted below
    // if the underlying call actually fails (e.g. sandboxing denies the login-item write).
    setLaunchAtLogin(next);
    try {
      if (next) {
        await enableAutostart();
      } else {
        await disableAutostart();
      }
    } catch (error) {
      console.error("Failed to update launch-at-login:", error);
      setLaunchAtLogin(!next);
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
          <span className="text-sm">Launch at login</span>
          <button
            type="button"
            role="switch"
            aria-checked={launchAtLogin}
            disabled={!loaded}
            onClick={toggleLaunchAtLogin}
            className={`relative h-5 w-9 shrink-0 rounded-full transition-colors disabled:opacity-50 ${
              launchAtLogin ? "bg-primary" : "bg-input"
            }`}
          >
            <span
              className={`absolute top-0.5 left-0.5 size-4 rounded-full bg-white shadow transition-transform ${
                launchAtLogin ? "translate-x-4" : "translate-x-0"
              }`}
            />
          </button>
        </label>
      </div>

      <div className="mt-auto flex flex-col items-center gap-1 border-t border-border p-4 text-center">
        <img src={logo} alt="" className="size-8" />
        <Wordmark className="text-sm" />
        <span className="text-muted-foreground text-xs">
          Version {pkg.version}
        </span>
      </div>
    </div>
  );
}
