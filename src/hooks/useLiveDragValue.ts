import { useCallback, useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { useSmoothedNumber } from "./useSmoothedNumber";

/**
 * Tracks a slider's own live drag position, decoupled from whatever asynchronous/round-trip
 * `targetValue` eventually confirms it (a Zustand store update, a backend push, etc).
 *
 * Two things this buys, both confirmed to matter for a fast drag feeling truly smooth and not
 * costing more CPU than it has to:
 * 1. Nothing downstream of `targetValue` (a store update, a parent re-render) needs to happen on
 *    every tick just to keep the thumb moving -- the caller can update it as rarely as it likes
 *    (even only once, when the drag ends) and the slider still tracks the pointer 1:1 in between.
 * 2. `updateDrag` doesn't commit a React state update (and therefore a re-render) for every raw
 *    pointer tick -- it only records the latest value and, if one isn't already pending, schedules
 *    a single `requestAnimationFrame` to commit it. A raw pointer event stream can fire faster than
 *    the display can even paint (confirmed as a real, measurable source of high CPU during
 *    dragging, not just theoretical waste); collapsing however many ticks land within one frame
 *    into a single state update caps re-renders at the display's own refresh rate, which is the
 *    fastest anything could possibly be *seen* to update anyway.
 */
export function useLiveDragValue(targetValue: number) {
  const [isDragging, setIsDragging] = useState(false);
  const [liveValue, setLiveValue] = useState<number | null>(null);
  // The latest tick's value, updated synchronously (no re-render) on every call -- `liveValue`
  // (state) can lag this by up to one animation frame while a batched commit is pending.
  const pendingValueRef = useRef<number | null>(null);
  const rafIdRef = useRef<number | null>(null);

  const cancelPendingFrame = useCallback(() => {
    if (rafIdRef.current !== null) {
      cancelAnimationFrame(rafIdRef.current);
      rafIdRef.current = null;
    }
  }, []);

  // Ends the drag, returning the true final value. Deliberately does *not* just read
  // `pendingValueRef.current` and call the normal (batched) `setLiveValue`/`setIsDragging` --
  // confirmed live as the cause of a fast drag visibly snapping to a stale position before
  // correcting to the real one on release: React 18 batches state updates from the same event
  // handler into a single render, so `setLiveValue(final)` and `setIsDragging(false)` together
  // would skip straight from whatever the *previous* rAF-batched tick left `useSmoothedNumber`'s
  // internal position tracker at, directly to non-dragging mode -- the tracker never gets a
  // render of its own to actually catch up to this final tick first. The gap between "previous
  // committed tick" and "true final tick" grows with drag speed (ticks are further apart in
  // value), which is exactly why this got more visible the faster the drag was.
  // `flushSync` forces that catch-up render to happen for real, synchronously, before anything
  // else -- a real React API for exactly this "hand off a final value before switching modes"
  // situation, not a hack. It's more expensive than a normal batched update, but this only runs
  // once per drag gesture (on release), never per tick, so that cost doesn't matter here.
  const flushFinalValue = useCallback((): number | null => {
    cancelPendingFrame();
    const final = pendingValueRef.current;
    pendingValueRef.current = null;
    if (final !== null) {
      flushSync(() => {
        setLiveValue(final);
      });
    }
    return final;
  }, [cancelPendingFrame]);

  // Unmounting mid-drag (e.g. the app the session belongs to quits while its slider is being
  // dragged) would otherwise leave a scheduled frame to fire later and call `setLiveValue` on a
  // component that's gone -- React 18 just silently no-ops that rather than warning, so this
  // isn't a crash risk, but there's no reason to let a stray callback and its closure hang around
  // in the meantime either.
  useEffect(() => cancelPendingFrame, [cancelPendingFrame]);

  // Belt-and-suspenders beyond onPointerUp/onPointerCancel on the slider itself: if the browser
  // ever fails to deliver one of those (pointer capture released oddly, focus lost mid-drag), this
  // would otherwise get permanently stuck "dragging" -- silently freezing this control's easing
  // forever. A window-level listener can't miss the pointer going up anywhere on screen.
  useEffect(() => {
    if (!isDragging) {
      return;
    }
    const stopDragging = () => {
      flushFinalValue();
      setIsDragging(false);
      setLiveValue(null);
    };
    window.addEventListener("pointerup", stopDragging);
    window.addEventListener("pointercancel", stopDragging);
    return () => {
      window.removeEventListener("pointerup", stopDragging);
      window.removeEventListener("pointercancel", stopDragging);
    };
  }, [isDragging, flushFinalValue]);

  // `useSmoothedNumber` returns `target` straight back (synchronously, no extra render) whenever
  // `instant` is true -- see that hook's own doc comment -- so this already *is* `liveValue`
  // while dragging, with no separate fallback needed here.
  const displayValue = useSmoothedNumber(
    isDragging && liveValue !== null ? liveValue : targetValue,
    isDragging,
  );

  return {
    displayValue,
    isDragging,
    beginDrag: () => setIsDragging(true),
    /** Call on every `onValueChange` tick while dragging. */
    updateDrag: (next: number) => {
      pendingValueRef.current = next;
      if (rafIdRef.current === null) {
        rafIdRef.current = requestAnimationFrame(() => {
          rafIdRef.current = null;
          setLiveValue(pendingValueRef.current);
        });
      }
    },
    /** Call on `onPointerUp`/`onPointerCancel`. Returns the final live value to commit upstream
     * (or `null` if the drag ended with no ticks, e.g. a same-position click-release). */
    endDrag: (): number | null => {
      const final = flushFinalValue();
      setIsDragging(false);
      setLiveValue(null);
      return final;
    },
  };
}
