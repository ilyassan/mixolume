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
  /** Every device id -> name ever seen, including ones no longer plugged in -- see
   * `MixerState.knownDeviceNames`'s own doc comment. */
  knownDeviceNames: Record<string, string>;
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
  knownDeviceNames,
  disabled,
  onChange,
}: OutputDevicePickerProps) {
  // Defensive: a device this session is routed to but that's no longer in the current list
  // (unplugged since, or the OS's own Settings panel routed it somewhere this poll hasn't caught
  // up to reporting yet) would otherwise leave nothing matching the current value at all -- a
  // synthetic item keeps the control legible instead of silently showing a blank trigger. Its
  // label prefers the device's real, previously-seen name ("Headphones (disconnected)") over a
  // bare "Unknown device" -- almost always available in practice, since a device has to have
  // been plugged in and enumerated at least once before any session could have been routed to
  // it in the first place.
  const hasCurrentDevice =
    outputDeviceId === null || devices.some((device) => device.id === outputDeviceId);
  const missingDeviceLabel =
    outputDeviceId && !hasCurrentDevice
      ? knownDeviceNames[outputDeviceId]
        ? `${knownDeviceNames[outputDeviceId]} (disconnected)`
        : "Unknown device"
      : null;

  return (
    <div className="flex items-center gap-2 px-2.5 pb-2.5">
      <Headphones className="text-muted-foreground size-3.5 shrink-0" />
      <Select
        value={outputDeviceId ?? SYSTEM_DEFAULT_VALUE}
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
          {missingDeviceLabel && outputDeviceId && (
            <SelectItem value={outputDeviceId}>{missingDeviceLabel}</SelectItem>
          )}
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
