import { memo, useEffect, useState } from "react";
import { ChevronRight, Volume2, VolumeX } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { Slider } from "@/components/ui/slider";
import { Button } from "@/components/ui/button";
import { SessionIcon } from "@/components/SessionIcon";
import { toLeftRight, fromLeftRight } from "@/lib/balance";
import { useSmoothedNumber } from "@/hooks/useSmoothedNumber";
import type { FadingSession } from "@/hooks/useSessionListWithFadeOut";

interface SessionRowProps {
  session: FadingSession;
  onVolumeChange: (sessionId: string, volume: number) => void;
  onMuteToggle: (sessionId: string, muted: boolean) => void;
  onBalanceChange: (sessionId: string, balance: number) => void;
}

// Memoized: `useSessionListWithFadeOut` now reuses the same `session` object reference for a
// row whose data hasn't actually changed (see that hook's own comment), and the mixer store's
// action functions (`onVolumeChange` etc.) are stable Zustand references that never change
// identity -- so a plain shallow-prop compare correctly skips re-rendering every *other* row
// whenever one row's volume/duck state updates, instead of the whole list re-rendering (and
// re-running each row's own layout/animation work) on every drag tick or 150ms poll. Confirmed
// live as a real, measurable source of CPU spikes and dropped frames while dragging, not just a
// theoretical inefficiency.
export const SessionRow = memo(function SessionRow({
  session,
  onVolumeChange,
  onMuteToggle,
  onBalanceChange,
}: SessionRowProps) {
  const {
    id,
    displayName,
    iconPng,
    volume,
    effectiveVolume,
    muted,
    balance,
    isActive,
    isDuckTrigger,
    isDucked,
    removing,
  } = session;
  // What's actually coming out right now, not the target -- a ducked app should visibly read
  // quieter, not sit at its full set volume while audibly playing much lower than that.
  const percent = Math.round(effectiveVolume * 100);
  // Advanced panel (balance, and room for whatever gets added later) stays collapsed by
  // default -- opened automatically only if a row already has a non-center balance (e.g. after
  // a relaunch), so returning users don't lose sight of a setting they already made.
  const [expanded, setExpanded] = useState(balance !== 0);

  const [left, right] = toLeftRight(volume, balance);
  const leftPercent = Math.round(left * 100);
  const rightPercent = Math.round(right * 100);

  // Distinguishes "the user is dragging this slider right now" (should track the cursor with no
  // added lag) from "this changed for some other reason" (auto-duck lowering/restoring it, most
  // notably -- should ease smoothly instead of jumping). Driven straight off the slider's own
  // pointer interaction, not inferred from the value itself.
  const [isDraggingVolume, setIsDraggingVolume] = useState(false);
  // Belt-and-suspenders beyond the Slider's own onPointerUp/onPointerCancel: those are attached
  // to the slider element itself, and if the browser ever fails to deliver one to it (pointer
  // capture released oddly, focus lost mid-drag, etc.) this would otherwise get permanently
  // stuck "dragging" -- which silently disables smoothing for that one row forever, exactly the
  // "some apps never animate" symptom this was confirmed to cause live. A window-level listener
  // can't miss the pointer going up anywhere on screen.
  useEffect(() => {
    if (!isDraggingVolume) {
      return;
    }
    const stopDragging = () => setIsDraggingVolume(false);
    window.addEventListener("pointerup", stopDragging);
    window.addEventListener("pointercancel", stopDragging);
    return () => {
      window.removeEventListener("pointerup", stopDragging);
      window.removeEventListener("pointercancel", stopDragging);
    };
  }, [isDraggingVolume]);
  const displayPercent = useSmoothedNumber(percent, isDraggingVolume);

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

  // Motion owns opacity entirely here (mount fade-in, the `removing` fade-out, and the
  // active/inactive dim) -- it used to be split between this and a CSS class, which meant two
  // different systems fighting over the same property. `layout="position"` (not the default
  // `layout` boolean, which also tracks size): an earlier version used full layout tracking
  // combined with `layoutId` for a cross-section shared transition, and confirmed live that the
  // combination made the row visibly flicker (fade out and back in) on *any* value update, not
  // just when a session actually appeared/disappeared -- matches a documented Framer Motion
  // pitfall (`layoutId` inside `AnimatePresence`, alongside size-affecting children like the
  // badges, triggers layout and enter/exit animation simultaneously). Position-only tracking
  // still animates reordering within a list smoothly without touching size at all, so there's
  // nothing left for a sibling's width change to disturb.
  const targetOpacity = removing ? 0 : isActive ? 1 : 0.5;

  return (
    <motion.div
      layout="position"
      initial={{ opacity: 0 }}
      animate={{ opacity: targetOpacity }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.3, ease: "easeOut" }}
      className={
        "rounded-xl bg-card/60 transition-[background-color] duration-500 ease-out " +
        (removing ? "pointer-events-none" : "")
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

          <AnimatePresence initial={false}>
            {isDuckTrigger && (
              <motion.span
                key="trigger-badge"
                layout
                initial={{ opacity: 0, scale: 0.8 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.8 }}
                transition={{ duration: 0.15, ease: "easeOut" }}
                className="shrink-0 rounded-full bg-primary/15 px-1.5 py-0.5 text-[10px] font-medium text-primary"
                title="Auto-duck: everything else is quieter because of this app right now"
              >
                Active
              </motion.span>
            )}
            {isDucked && (
              <motion.span
                key="ducked-badge"
                layout
                initial={{ opacity: 0, scale: 0.8 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.8 }}
                transition={{ duration: 0.15, ease: "easeOut" }}
                className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground"
                title="Auto-duck: temporarily lowered because another app is active"
              >
                Lowered
              </motion.span>
            )}
          </AnimatePresence>

          <Button
            type="button"
            variant="ghost"
            size="icon"
            disabled={removing}
            aria-pressed={muted}
            aria-label={muted ? `Unmute ${displayName}` : `Mute ${displayName}`}
            onClick={() => onMuteToggle(id, !muted)}
            className={
              "relative overflow-hidden " +
              (muted ? "text-destructive" : "text-muted-foreground")
            }
          >
            <AnimatePresence mode="popLayout" initial={false}>
              {muted ? (
                <motion.span
                  key="muted"
                  initial={{ opacity: 0, scale: 0.6 }}
                  animate={{ opacity: 1, scale: 1 }}
                  exit={{ opacity: 0, scale: 0.6 }}
                  transition={{ duration: 0.12, ease: "easeOut" }}
                  className="inline-flex"
                >
                  <VolumeX className="size-4" />
                </motion.span>
              ) : (
                <motion.span
                  key="unmuted"
                  initial={{ opacity: 0, scale: 0.6 }}
                  animate={{ opacity: 1, scale: 1 }}
                  exit={{ opacity: 0, scale: 0.6 }}
                  transition={{ duration: 0.12, ease: "easeOut" }}
                  className="inline-flex"
                >
                  <Volume2 className="size-4" />
                </motion.span>
              )}
            </AnimatePresence>
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
            <motion.span
              animate={{ rotate: expanded ? 90 : 0 }}
              transition={{ duration: 0.15, ease: "easeOut" }}
              className="inline-flex"
            >
              <ChevronRight className="size-3.5" />
            </motion.span>
          </Button>
        </div>

        <div className="mt-1.5 flex items-center gap-2">
          <Slider
            aria-label={`${displayName} volume`}
            value={[displayPercent]}
            min={0}
            max={100}
            step={1}
            disabled={removing}
            onValueChange={([next]) => onVolumeChange(id, next / 100)}
            onPointerDown={() => setIsDraggingVolume(true)}
            onPointerUp={() => setIsDraggingVolume(false)}
            onPointerCancel={() => setIsDraggingVolume(false)}
            className="flex-1"
          />
          <span className="text-muted-foreground w-9 shrink-0 text-right text-xs tabular-nums">
            {Math.round(displayPercent)}%
          </span>
        </div>
      </div>

      <AnimatePresence initial={false}>
        {expanded && (
          <motion.div
            key="lr-panel"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.18, ease: "easeOut" }}
            className="overflow-hidden"
          >
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
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  );
});
