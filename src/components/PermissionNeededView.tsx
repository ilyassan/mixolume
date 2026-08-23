import { ShieldAlert } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "@/components/ui/button";

const SCREEN_RECORDING_SETTINGS_URL =
  "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

export function PermissionNeededView() {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 py-8 text-center">
      <ShieldAlert className="text-muted-foreground size-8" />
      <p className="text-sm font-medium">Permission needed</p>
      <p className="text-muted-foreground text-xs">
        MiXolume needs Screen & System Audio Recording access to detect which
        apps are playing sound.
      </p>
      <Button
        size="default"
        onClick={() => {
          void openUrl(SCREEN_RECORDING_SETTINGS_URL);
        }}
      >
        Open System Settings
      </Button>
      <p className="text-muted-foreground text-xs">
        After granting access, quit and reopen MiXolume -- this permission
        only takes effect on the next launch.
      </p>
    </div>
  );
}
