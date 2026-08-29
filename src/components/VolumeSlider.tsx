import { Slider } from "@/components/ui/slider";
import { useLiveDragValue } from "@/hooks/useLiveDragValue";
import { useThrottledCallback } from "@/hooks/useThrottledCallback";
import { useDraggingSessionFreeze } from "@/hooks/useDraggingSessionFreeze";
import { setVolume as setVolumeCommand } from "@/lib/tauri";

// ~25Hz -- still fast enough that consecutive audio updates blend into what sounds like a
// continuous change for any drag speed a human actually does by hand (each step is well under a
// percentage point apart at normal drag speeds), while being under half the IPC call volume 60Hz
// was. An experiment, not a confirmed fix: WKWebView's IPC bridge to the Rust side is documented
// as generally less optimized than Chromium/WebView2's (see the researched comparison this
// project already has elsewhere), so it's plausible call *frequency* itself was contributing to
// the residual choppiness reported even after every other lever was pulled -- but it hasn't been
// isolated and confirmed the way the earlier fixes in this file were, so this number is a
// starting point to react to live feedback, not a number arrived at from a measurement.
const BACKEND_UPDATE_INTERVAL_MS = 40;

interface VolumeSliderProps {
  sessionId: string;
  displayName: string;
  /** `effectiveVolume * 100`, unrounded -- see `SessionRow.tsx` for why unrounded. */
  percent: number;
  maxVolumePercent: number;
  disabled: boolean;
  muted: boolean;
  /** The mixer store's `setVolume` action -- kept out of the hot per-tick drag path (see below)
   * and called only for changes that aren't an active pointer drag, plus once more at the end of
   * one, to reconcile the store's canonical state. */
  onVolumeChange: (sessionId: string, volume: number) => void;
  /** Called once, the moment the user starts dragging this slider while muted -- manually moving
   * the volume is treated as unmuting, an alternative to (not a replacement for) the mute button
   * itself. `volume` on the backend is never touched by muting, only this component's *display*
   * (forced to 0 below) is, so clicking the mute button to unmute still restores exactly where it
   * was; dragging instead commits wherever the user actually drags to. */
  onUnmute: () => void;
}

/**
 * Own component, not inlined in `SessionRow.tsx`, specifically so a drag gesture here only
 * re-renders this small plain `<div>` -- not the row's own `motion.div` wrapper (`layout=
 * "position"`), which re-measures the DOM on every render it's involved in. That matters because
 * of what this component deliberately does *not* do on every drag tick: call the mixer store's
 * `setVolume` action.
 *
 * That action rebuilds the whole `sessions` array (a fresh object for the dragged session, via
 * `state.sessions.map(...)`) on every call -- necessary for its optimistic-update guarantee, but
 * it means calling it on every `pointermove` tick would give this row a new `session` prop every
 * tick too, defeating `React.memo` and forcing the row (Motion layout-tracking and all) through a
 * full re-render dozens of times a second. Instead, while a pointer drag is in progress, this
 * calls the raw `setVolume` Tauri command directly (throttled -- see `BACKEND_UPDATE_INTERVAL_MS`)
 * for real audible feedback without round-tripping the store. The store is only told about the
 * change (`onVolumeChange`) once the gesture ends, to reconcile its canonical state; a keyboard-
 * driven change (no pointer drag involved) goes through the store on every change as before, since
 * there's no high-frequency tick to protect against there.
 */
export function VolumeSlider({
  sessionId,
  displayName,
  percent,
  maxVolumePercent,
  disabled,
  muted,
  onVolumeChange,
  onUnmute,
}: VolumeSliderProps) {
  // Muted reads as 0, not wherever `volume` happens to be set -- the backend's stored `volume` is
  // untouched (see `onUnmute`'s doc comment above), this is purely how it's displayed.
  const { displayValue, isDragging, beginDrag, updateDrag, endDrag } = useLiveDragValue(
    muted ? 0 : percent,
  );

  // Watches `isDragging` itself, rather than being wired into the Slider's own
  // onPointerDown/Up/Cancel individually, so this also covers `useLiveDragValue`'s window-level
  // backstop (a missed pointerup) -- both paths change `isDragging`, so both are covered here.
  useDraggingSessionFreeze(sessionId, isDragging);

  const sendVolumeThrottled = useThrottledCallback((nextPercent: number) => {
    setVolumeCommand(sessionId, nextPercent / 100).catch((error) => {
      console.error(`Failed to set volume for ${sessionId}:`, error);
    });
  }, BACKEND_UPDATE_INTERVAL_MS);

  return (
    <div className="mt-1.5 flex items-center gap-2">
      <Slider
        aria-label={`${displayName} volume`}
        value={[displayValue]}
        min={0}
        max={maxVolumePercent}
        // Finer than the 1%-per-step default -- whole-percent steps read as faintly hopping
        // rather than gliding during a slow, deliberate drag. Not finer than that, though: at
        // this row's actual on-screen width, 0.1% is already well under a single physical pixel
        // of movement even on a high-DPI display, so anything smaller only means more distinct
        // `onValueChange` firings -- and the real (if small) Radix-internal layout work that
        // comes with each one -- for changes nobody could ever actually see. The percent readout
        // below still rounds to a whole number either way, so this only affects how continuous
        // the motion feels, not what's displayed or the precision the backend receives.
        step={0.1}
        disabled={disabled}
        onValueChange={([next]) => {
          if (isDragging) {
            updateDrag(next);
            sendVolumeThrottled(next);
          } else {
            // Not a pointer drag (e.g. arrow-key adjustment) -- no high-frequency tick to
            // protect against, so go through the store normally like any other change.
            if (muted) {
              onUnmute();
            }
            onVolumeChange(sessionId, next / 100);
          }
        }}
        onPointerDown={() => {
          if (muted) {
            onUnmute();
          }
          beginDrag();
        }}
        onPointerUp={() => {
          const final = endDrag();
          if (final !== null) {
            onVolumeChange(sessionId, final / 100);
          }
        }}
        onPointerCancel={() => {
          const final = endDrag();
          if (final !== null) {
            onVolumeChange(sessionId, final / 100);
          }
        }}
        className="flex-1"
      />
      <span className="text-muted-foreground w-9 shrink-0 text-right text-xs tabular-nums">
        {Math.round(displayValue)}%
      </span>
    </div>
  );
}
