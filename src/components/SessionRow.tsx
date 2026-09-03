import { memo, useState } from "react";
import { ChevronRight, Volume2, VolumeX } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { Button } from "@/components/ui/button";
import { SessionIcon } from "@/components/SessionIcon";
import { VolumeSlider } from "@/components/VolumeSlider";
import { BalanceSliders } from "@/components/BalanceSliders";
import { OutputDevicePicker } from "@/components/OutputDevicePicker";
import { useMixerStore } from "@/stores/mixer-store";
import type { FadingSession } from "@/hooks/useSessionListWithFadeOut";
import type { OutputDevice } from "@/lib/tauri";

interface SessionRowProps {
  session: FadingSession;
  onVolumeChange: (sessionId: string, volume: number) => void;
  onMuteToggle: (sessionId: string, muted: boolean) => void;
  onBalanceChange: (sessionId: string, balance: number) => void;
  /** Highest volume percent the current backend allows -- 100 normally, 200 on macOS's boosted
   * backend. Sizes the main volume slider's `max`; the L/R balance sliders stay 0-100 always,
   * since balance is a pan ratio, not itself boostable. */
  maxVolumePercent: number;
  /** Whether the current backend can route this session's audio to a specific output device --
   * currently Windows only. `outputDevices`/`onOutputDeviceChange` are only ever used when this
   * is true. */
  outputRoutingSupported: boolean;
  outputDevices: OutputDevice[];
  /** Every output device id -> name ever seen, including ones no longer plugged in -- lets the
   * picker label a since-unplugged device by name instead of a bare "Unknown device". */
  knownDeviceNames: Record<string, string>;
  onOutputDeviceChange: (sessionId: string, deviceId: string | null) => void;
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
  maxVolumePercent,
  outputRoutingSupported,
  outputDevices,
  knownDeviceNames,
  onOutputDeviceChange,
}: SessionRowProps) {
  const {
    id,
    displayName,
    iconPng,
    effectiveVolume,
    muted,
    balance,
    isActive,
    isDuckTrigger,
    isDucked,
    outputDeviceId,
    removing,
  } = session;
  // What's actually coming out right now, not the target -- a ducked app should visibly read
  // quieter, not sit at its full set volume while audibly playing much lower than that. Kept
  // unrounded here (rounded only where it's actually displayed, in the `%` label below) --
  // rounding this to a whole number fed a quantized value straight into the slider's controlled
  // `value`, which combined with the Slider's own step to make dragging visibly hop between
  // whole percents instead of gliding with the pointer, confirmed live.
  const percent = effectiveVolume * 100;
  // Advanced panel (balance, output device routing) stays collapsed by default -- opened
  // automatically only if a row already has a non-center balance or a non-default output device
  // set (e.g. after a relaunch), so returning users don't lose sight of a setting they already
  // made.
  const [expanded, setExpanded] = useState(balance !== 0 || outputDeviceId !== null);
  // Whether *any* row (not necessarily this one) is currently being drag-adjusted -- see
  // `layout` below for why this row needs to know about every other row's drag state, not just
  // its own.
  const isAnyRowDragging = useMixerStore((state) => state.draggingSessionId !== null);

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
  //
  // Suspended entirely (`layout={false}`) while *any* row is being drag-adjusted, regardless of
  // which one: Framer Motion's layout-projection system tracks every mounted `layout`-enabled
  // node in one shared registry, not per-component -- when any one of them updates, it schedules
  // a measurement pass across the *entire* registered set in that same animation frame (this is
  // what makes relative-position FLIP math correct, not a bug). `React.memo` on this component
  // (see its own doc comment) stops *React* from re-rendering unrelated rows, but does nothing
  // about this -- Framer Motion's registry isn't keyed off React's render decisions. Confirmed
  // live with a bare, zero-React-state `<input type="range">` that called the raw backend
  // command directly (bypassing this entire component's own state/store/animation code): it
  // glitched identically to the real slider purely from the resulting real volume-state pushes
  // reflowing sibling rows, and stopped glitching the moment those pushes stopped arriving --
  // narrowing the cause to exactly this, not the IPC round-trip or this app's drag architecture.
  // Reordering (the thing `layout="position"` actually animates) only happens when a session
  // appears/disappears/renames, never merely from a value changing, so losing it for the
  // relatively brief span of an active drag has no real downside.
  const targetOpacity = removing ? 0 : isActive ? 1 : 0.5;

  return (
    <motion.div
      layout={isAnyRowDragging ? false : "position"}
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

        <VolumeSlider
          sessionId={id}
          displayName={displayName}
          percent={percent}
          maxVolumePercent={maxVolumePercent}
          disabled={removing}
          muted={muted}
          onVolumeChange={onVolumeChange}
          onUnmute={() => onMuteToggle(id, false)}
          onMute={() => onMuteToggle(id, true)}
        />
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
            <BalanceSliders
              sessionId={id}
              displayName={displayName}
              balance={balance}
              muted={muted}
              disabled={removing}
              onBalanceChange={onBalanceChange}
              onUnmute={() => onMuteToggle(id, false)}
            />
            {outputRoutingSupported && (
              <OutputDevicePicker
                sessionId={id}
                displayName={displayName}
                outputDeviceId={outputDeviceId}
                devices={outputDevices}
                knownDeviceNames={knownDeviceNames}
                disabled={removing}
                onChange={onOutputDeviceChange}
              />
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  );
});
