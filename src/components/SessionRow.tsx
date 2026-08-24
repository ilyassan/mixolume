import { useState } from "react";
import { ChevronDown, ChevronRight, Volume2, VolumeX } from "lucide-react";
import { Slider } from "@/components/ui/slider";
import { Button } from "@/components/ui/button";
import { SessionIcon } from "@/components/SessionIcon";
import { toLeftRight, fromLeftRight } from "@/lib/balance";
import type { FadingSession } from "@/hooks/useSessionListWithFadeOut";

interface SessionRowProps {
  session: FadingSession;
  onVolumeChange: (sessionId: string, volume: number) => void;
  onMuteToggle: (sessionId: string, muted: boolean) => void;
  onBalanceChange: (sessionId: string, balance: number) => void;
}

export function SessionRow({
  session,
  onVolumeChange,
  onMuteToggle,
  onBalanceChange,
}: SessionRowProps) {
  const { id, displayName, iconPng, volume, muted, balance, isActive, removing } =
    session;
  const percent = Math.round(volume * 100);
  // Advanced panel (balance, and room for whatever gets added later) stays collapsed by
  // default -- opened automatically only if a row already has a non-center balance (e.g. after
  // a relaunch), so returning users don't lose sight of a setting they already made.
  const [expanded, setExpanded] = useState(balance !== 0);

  const [left, right] = toLeftRight(volume, balance);
  const leftPercent = Math.round(left * 100);
  const rightPercent = Math.round(right * 100);

  const handleLeftChange = (nextPercent: number) => {
    const [newVolume, newBalance] = fromLeftRight(nextPercent / 100, right);
    onVolumeChange(id, newVolume);
    onBalanceChange(id, newBalance);
  };
  const handleRightChange = (nextPercent: number) => {
    const [newVolume, newBalance] = fromLeftRight(left, nextPercent / 100);
    onVolumeChange(id, newVolume);
    onBalanceChange(id, newBalance);
  };

  return (
    <div
      className={
        "rounded-xl bg-card/60 transition-[opacity,background-color] duration-500 ease-out " +
        (removing
          ? "pointer-events-none opacity-0"
          : isActive
            ? "opacity-100"
            : "opacity-50")
      }
      data-session-id={id}
      data-active={isActive}
      data-removing={removing}
    >
      <div className="p-2.5">
        <div className="flex items-center gap-3">
          <SessionIcon iconPng={iconPng} displayName={displayName} />

          <p className="min-w-0 flex-1 truncate text-sm font-medium leading-tight">
            {displayName}
          </p>

          <Button
            type="button"
            variant="ghost"
            size="icon"
            disabled={removing}
            aria-pressed={muted}
            aria-label={muted ? `Unmute ${displayName}` : `Mute ${displayName}`}
            onClick={() => onMuteToggle(id, !muted)}
            className={muted ? "text-destructive" : "text-muted-foreground"}
          >
            {muted ? (
              <VolumeX className="size-4" />
            ) : (
              <Volume2 className="size-4" />
            )}
          </Button>

          <Button
            type="button"
            variant="ghost"
            size="icon"
            disabled={removing}
            aria-expanded={expanded}
            aria-label={
              expanded
                ? `Hide advanced controls for ${displayName}`
                : `Show advanced controls for ${displayName}`
            }
            onClick={() => setExpanded((v) => !v)}
            className="text-muted-foreground -ml-1"
          >
            {expanded ? (
              <ChevronDown className="size-3.5" />
            ) : (
              <ChevronRight className="size-3.5" />
            )}
          </Button>
        </div>

        <div className="mt-1.5 flex items-center gap-2">
          <Slider
            aria-label={`${displayName} volume`}
            value={[percent]}
            min={0}
            max={100}
            step={1}
            disabled={removing}
            onValueChange={([next]) => onVolumeChange(id, next / 100)}
            className="flex-1"
          />
          <span className="text-muted-foreground w-9 shrink-0 text-right text-xs tabular-nums">
            {percent}%
          </span>
        </div>
      </div>

      {expanded && (
        <div className="mt-1.5 flex items-center gap-4 px-2.5 pb-2.5">
          <div className="flex flex-1 items-center gap-2">
            <span className="text-muted-foreground w-3 text-[10px] font-medium">
              L
            </span>
            <Slider
              aria-label={`${displayName} left channel`}
              value={[leftPercent]}
              min={0}
              max={100}
              step={1}
              disabled={removing}
              onValueChange={([next]) => handleLeftChange(next)}
              className="flex-1"
            />
          </div>
          <div className="flex flex-1 items-center gap-2">
            <span className="text-muted-foreground w-3 text-[10px] font-medium">
              R
            </span>
            <Slider
              aria-label={`${displayName} right channel`}
              value={[rightPercent]}
              min={0}
              max={100}
              step={1}
              disabled={removing}
              onValueChange={([next]) => handleRightChange(next)}
              className="flex-1"
            />
          </div>
        </div>
      )}
    </div>
  );
}
