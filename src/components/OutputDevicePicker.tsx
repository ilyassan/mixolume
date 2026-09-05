import { Headphones } from "lucide-react";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { OutputDevice } from "@/lib/tauri";

/** The `<Select>` value that means "system default" -- kept distinct from `null` (which is what
 * actually gets sent to the backend for it) since Radix `Select.Item` values are always strings,
 * same reason the previous plain-`<select>` version needed this. */
const SYSTEM_DEFAULT_VALUE = "system-default";

interface OutputDevicePickerProps {
  sessionId: string;
  displayName: string;
  outputDeviceId: string | null;
  devices: OutputDevice[];
  disabled: boolean;
  onChange: (sessionId: string, deviceId: string | null) => void;
}

/**
 * Per-app output device routing -- lets this one app's audio play through a different device
 * (e.g. headphones) while everything else stays on the system default, the same feature Windows'
 * own Settings > Sound > Volume mixer exposes per-app. Only rendered by `SessionRow` when the
 * backend reports `outputRoutingSupported` (currently Windows only).
 */
export function OutputDevicePicker({
  sessionId,
  displayName,
  outputDeviceId,
  devices,
  disabled,
  onChange,
}: OutputDevicePickerProps) {
  // A device this session was routed to but that's no longer in the current list (unplugged
  // since) shows as "System default" rather than a stale, unselectable "disconnected" entry --
  // the backend independently clears the routing choice itself once it notices the device is
  // gone (so this is also what the very next push will report), this just avoids a beat of
  // showing a dead option in the meantime.
  const hasCurrentDevice =
    outputDeviceId !== null && devices.some((device) => device.id === outputDeviceId);
  const selectValue = hasCurrentDevice ? outputDeviceId : SYSTEM_DEFAULT_VALUE;

  return (
    <div className="flex items-center gap-2 px-2.5 pb-2.5">
      <Headphones className="text-muted-foreground size-3.5 shrink-0" />
      <Select
        value={selectValue}
        disabled={disabled}
        onValueChange={(value) => {
          onChange(sessionId, value === SYSTEM_DEFAULT_VALUE ? null : value);
        }}
      >
        <SelectTrigger
          aria-label={`Output device for ${displayName}`}
          className="flex-1"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={SYSTEM_DEFAULT_VALUE}>System default</SelectItem>
          {devices.map((device) => (
            <SelectItem key={device.id} value={device.id}>
              {device.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}
