import { useRef } from "react";

/**
 * Wraps `fn` so it's actually called at most once per `intervalMs`, dropping calls in between --
 * built for a drag's `onValueChange`, which can fire far faster than a backend/audio update
 * needs to land to sound and feel smooth (human perception has no use for volume changes faster
 * than screen refresh rate) but costs real, synchronous main-thread work every time it does (IPC
 * serialization) that's better spent on rendering the drag itself. Callers are still responsible
 * for a final, unthrottled call once the drag ends, so the last few dropped ticks aren't lost --
 * this is for the *continuous* feedback during the gesture, not its final committed value.
 *
 * The call itself is deferred via `setTimeout(..., 0)` rather than made inline from the
 * triggering event handler: `onValueChange` fires as part of the same synchronous task as the
 * pointermove event that's also driving this frame's slider position update, so anything run
 * inline here -- including just the synchronous portion of `invoke()` before it hands off to
 * the native bridge -- competes with that same task for the frame's paint budget. Pushing it to
 * its own macrotask lets the browser get the current frame's visual update out first.
 */
export function useThrottledCallback<Args extends unknown[]>(
  fn: (...args: Args) => void,
  intervalMs: number,
): (...args: Args) => void {
  const lastCallRef = useRef(0);
  return (...args: Args) => {
    const now = performance.now();
    if (now - lastCallRef.current >= intervalMs) {
      lastCallRef.current = now;
      setTimeout(() => fn(...args), 0);
    }
  };
}
