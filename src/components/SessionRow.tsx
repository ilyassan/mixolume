import { useState } from "react";
import { Volume2, VolumeX } from "lucide-react";
import { Slider } from "@/components/ui/slider";
import { Button } from "@/components/ui/button";
import { SessionIcon } from "@/components/SessionIcon";
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
  // Balance defaults hidden -- most apps never need it, so it stays out of the way
  // unless a row already has one set (e.g. after a relaunch) or the user opens it.
  const [showBalance, setShowBalance] = useState(balance !== 0);

  return (
    <div
      className={
        "flex flex-col gap-1 rounded-lg px-3 py-2 transition-[opacity,transform] duration-500 ease-out " +
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
      <div className="flex items-center gap-3">
        <SessionIcon iconPng={iconPng} displayName={displayName} />

        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium leading-tight">
            {displayName}
          </p>
          <div className="mt-1 flex items-center gap-2">
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

        <Button
          type="button"
          variant="ghost"
          size="icon"
          disabled={removing}
          aria-pressed={showBalance}
          aria-label={
            showBalance
              ? `Hide balance control for ${displayName}`
              : `Show balance control for ${displayName}`
          }
          onClick={() => setShowBalance((v) => !v)}
          className="text-muted-foreground shrink-0 font-mono text-[10px]"
        >
          L/R
        </Button>

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
      </div>

      {showBalance && (
        <div className="flex items-center gap-2 pl-11">
          <span className="text-muted-foreground text-[10px]">L</span>
          <Slider
            aria-label={`${displayName} balance`}
            value={[Math.round(balance * 100)]}
            min={-100}
            max={100}
            step={1}
            disabled={removing}
            onValueChange={([next]) => onBalanceChange(id, next / 100)}
            className="flex-1"
          />
          <span className="text-muted-foreground text-[10px]">R</span>
        </div>
      )}
    </div>
  );
}
