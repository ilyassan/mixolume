import type { PointerEvent } from "react";
import { Slider } from "@/components/ui/slider";
import { balanceToChannels, balanceFromLeftFraction, balanceFromRightFraction } from "@/lib/balance";
import { useLiveDragValue } from "@/hooks/useLiveDragValue";
import { useThrottledCallback } from "@/hooks/useThrottledCallback";
import { useDraggingSessionFreeze } from "@/hooks/useDraggingSessionFreeze";
import { setBalance as setBalanceCommand } from "@/lib/tauri";

// Same rate as `VolumeSlider`'s -- see its own comment for the full reasoning (an experiment in
// reducing IPC call volume during a drag, not a confirmed fix).
const BACKEND_UPDATE_INTERVAL_MS = 40;

interface BalanceSlidersProps {
  sessionId: string;
  displayName: string;
  balance: number;
  muted: boolean;
  disabled: boolean;
  onBalanceChange: (sessionId: string, balance: number) => void;
  /** Called once, the moment the user starts dragging either slider while muted -- see
   * `VolumeSlider.tsx`'s identical prop for the full rationale. */
  onUnmute: () => void;
}

/**
 * Own component for the same reason `VolumeSlider` is its own component (see that file's doc
 * comment) -- a drag here only re-renders this small plain `<div>`, not the row's `motion.div`
 * wrapper, and the backend is only told about the change (throttled while dragging, once more,
 * unthrottled, when it ends) instead of round-tripping the store on every tick.
 *
 * L and R display and set balance's raw per-channel multiplier directly -- not volume-scaled, and
 * never touching `volume` itself -- see `@/lib/balance`'s module doc comment for the full
 * rationale (this used to be tangled with the volume slider; it isn't anymore). That also means
 * this component doesn't need `volume` at all, unlike its previous version.
 */
export function BalanceSliders({
  sessionId,
  displayName,
  balance,
  muted,
  disabled,
  onBalanceChange,
  onUnmute,
}: BalanceSlidersProps) {
  const [balanceLeft, balanceRight] = balanceToChannels(balance);
  // Muted reads as both channels at 0, same as the volume slider -- see `VolumeSlider.tsx`'s
  // identical treatment. `balance` itself is untouched underneath, so unmuting via the mute
  // button (not a drag) restores exactly where these were.
  const leftPercent = muted ? 0 : balanceLeft * 100;
  const rightPercent = muted ? 0 : balanceRight * 100;

  const leftDrag = useLiveDragValue(leftPercent);
  const rightDrag = useLiveDragValue(rightPercent);

  // See `VolumeSlider.tsx`'s identical use of this hook for why it exists. Either slider dragging
  // is enough to freeze this session's reference in the store -- both affect the same session's
  // balance.
  const isDragging = leftDrag.isDragging || rightDrag.isDragging;
  const { beginFreeze } = useDraggingSessionFreeze(sessionId, isDragging);

  const commitLeft = (nextLeftPercent: number) => {
    if (muted) {
      onUnmute();
    }
    onBalanceChange(sessionId, balanceFromLeftFraction(nextLeftPercent / 100));
  };
  const commitRight = (nextRightPercent: number) => {
    if (muted) {
      onUnmute();
    }
    onBalanceChange(sessionId, balanceFromRightFraction(nextRightPercent / 100));
  };

  const sendLeftThrottled = useThrottledCallback((nextLeftPercent: number) => {
    setBalanceCommand(sessionId, balanceFromLeftFraction(nextLeftPercent / 100)).catch(
      (error) => {
        console.error(`Failed to set balance for ${sessionId}:`, error);
      },
    );
  }, BACKEND_UPDATE_INTERVAL_MS);
  const sendRightThrottled = useThrottledCallback((nextRightPercent: number) => {
    setBalanceCommand(sessionId, balanceFromRightFraction(nextRightPercent / 100)).catch(
      (error) => {
        console.error(`Failed to set balance for ${sessionId}:`, error);
      },
    );
  }, BACKEND_UPDATE_INTERVAL_MS);

  // Whether the pointer actually grabbed the thumb, vs. landing elsewhere on the track -- see
  // `beginDrag`'s own doc comment in `useLiveDragValue` for why this changes how the gesture's
  // first tick is handled (instant live-tracking vs. easing to the click).
  const grabbedThumb = (event: PointerEvent) =>
    (event.target as HTMLElement).closest('[data-slot="slider-thumb"]') !== null;

  const beginLeftDrag = (event: PointerEvent) => {
    if (muted) {
      onUnmute();
    }
    // Synchronous, same event dispatch as `beginDrag()` -- see `beginFreeze`'s doc comment in
    // `useDraggingSessionFreeze` for why this can't wait for a `useEffect` to catch up.
    beginFreeze();
    leftDrag.beginDrag(grabbedThumb(event));
  };
  const beginRightDrag = (event: PointerEvent) => {
    if (muted) {
      onUnmute();
    }
    beginFreeze();
    rightDrag.beginDrag(grabbedThumb(event));
  };

  return (
    <div className="mt-1.5 flex items-center gap-4 px-2.5 pb-2.5">
      <div className="flex flex-1 items-center gap-2">
        <span className="text-muted-foreground w-3 text-[10px] font-medium">L</span>
        <Slider
          aria-label={`${displayName} left channel`}
          value={[leftDrag.displayValue]}
          min={0}
          max={100}
          // See `VolumeSlider.tsx`'s identical `step` for why 0.1 (not finer) -- already below a
          // single physical pixel of movement at this row's on-screen width.
          step={0.1}
          disabled={disabled}
          onValueChange={([next]) => {
            // `isDraggingNow()`, never the `isDragging` state -- see `VolumeSlider.tsx`'s
            // identical branch and `isDraggingNow`'s doc comment in `useLiveDragValue`.
            if (leftDrag.isDraggingNow()) {
              leftDrag.updateDrag(next);
              sendLeftThrottled(next);
            } else {
              leftDrag.commitInstant(next);
              commitLeft(next);
            }
          }}
          onPointerDown={beginLeftDrag}
          onPointerUp={() => {
            leftDrag.endDrag(commitLeft);
          }}
          onPointerCancel={() => {
            leftDrag.endDrag(commitLeft);
          }}
          className="flex-1"
        />
      </div>
      <div className="flex flex-1 items-center gap-2">
        <span className="text-muted-foreground w-3 text-[10px] font-medium">R</span>
        <Slider
          aria-label={`${displayName} right channel`}
          value={[rightDrag.displayValue]}
          min={0}
          max={100}
          // See `VolumeSlider.tsx`'s identical `step` for why 0.1 (not finer) -- already below a
          // single physical pixel of movement at this row's on-screen width.
          step={0.1}
          disabled={disabled}
          onValueChange={([next]) => {
            // See the left slider's identical branch above.
            if (rightDrag.isDraggingNow()) {
              rightDrag.updateDrag(next);
              sendRightThrottled(next);
            } else {
              rightDrag.commitInstant(next);
              commitRight(next);
            }
          }}
          onPointerDown={beginRightDrag}
          onPointerUp={() => {
            rightDrag.endDrag(commitRight);
          }}
          onPointerCancel={() => {
            rightDrag.endDrag(commitRight);
          }}
          className="flex-1"
        />
      </div>
    </div>
  );
}
