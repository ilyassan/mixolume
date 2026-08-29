import { useEffect, useRef, useState } from "react";

const EASED_DURATION_MS = 250;

/** Ease-out cubic -- fast start, gentle settle. No overshoot, which matters here: a volume
 * level animating past its target and bouncing back would read as a glitch, not a nice touch. */
function easeOutCubic(t: number): number {
  return 1 - (1 - t) ** 3;
}

/**
 * Smoothly eases a displayed number toward `target` whenever it changes, instead of jumping to
 * it immediately -- built for values that can change *programmatically* (e.g. auto-duck lowering
 * `effectiveVolume`), where an instant jump reads as a jarring glitch rather than a deliberate
 * user action.
 *
 * `instant` bypasses the easing entirely and returns `target` straight back, synchronously, with
 * no state update and no `requestAnimationFrame` involved -- pass `true` while the user is
 * actively dragging the control this feeds. This used to route the instant case through the same
 * RAF-driven `setState` the eased path uses (satisfying a React lint rule against calling
 * `setState` directly in an effect body), which meant every single call during a drag -- one per
 * pointer tick -- forced an *extra*, wasted re-render whose result was thrown away by every
 * caller that already had the live value in hand. Confirmed live as a real, measurable
 * contributor to high CPU during dragging, not just a theoretical inefficiency. The effect below
 * still runs on every instant call, but only to keep `currentRef` caught up (a plain ref write,
 * not a state update) so a *later* eased transition starts from the right place -- that's cheap
 * enough not to matter.
 *
 * Driving a Radix Slider's *value* through a single number like this (rather than CSS-
 * transitioning its internal `left`/`right` inline styles directly) sidesteps a real, confirmed
 * desync bug: the range fill and the thumb are two independent DOM elements, each transitioning
 * on its own -- under rapid updates (even an ordinary manual drag) they can visibly fall out of
 * lockstep. A single JS-driven number can't desync from itself: both elements read the exact same
 * value on the exact same render, every render.
 *
 * A plain `requestAnimationFrame` loop, not a Motion/Framer Motion value -- deliberately, after a
 * Motion-based `useSpring` approach turned out to have hard-to-predict behavior of its own (its
 * `duration` option is only a stiffness/damping *hint*, not a guaranteed duration). A small
 * self-contained RAF loop has no hidden library coupling to reason about.
 *
 * If this ever looks instant again despite the math here being correct, check for something
 * flooding stderr from the realtime audio thread first -- confirmed once already that blocking
 * I/O there (an old temporary debug print in `macos_ducking.rs`, since removed) delayed the
 * browser's own `requestAnimationFrame` callback by hundreds of milliseconds, which looks
 * identical to "the animation code doesn't work" from the outside.
 */
export function useSmoothedNumber(target: number, instant: boolean): number {
  const [display, setDisplay] = useState(target);
  // The value actually on screen right now, updated every frame -- read (not `display` state
  // directly) as the animation's starting point, so retargeting mid-animation continues smoothly
  // from wherever the animation currently is instead of restarting from the *previous target*.
  const currentRef = useRef(target);
  const frameRef = useRef<number | null>(null);

  useEffect(() => {
    if (frameRef.current !== null) {
      cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }

    // Just stay caught up -- no animation, no state update, so no re-render from this hook at
    // all while `instant` is true. `useLiveDragValue` returns `target` directly in this branch
    // (see below), never `display`, so there'd be nothing to render it for anyway.
    if (instant) {
      currentRef.current = target;
      return;
    }

    const from = currentRef.current;
    const to = target;
    if (from === to) {
      // Nothing to animate -- but `display` may still be stale from a preceding instant phase,
      // which never touches it (see above). Confirmed live as the actual cause of a drag release
      // visibly snapping back to a stale position before "correcting": releasing a drag commonly
      // lands exactly on the value already tracked in `currentRef` (that's the whole point of
      // keeping it in sync throughout the drag), which hit this exact branch and left `display`
      // holding whatever it was from *before* the drag ever started -- since nothing forces this
      // effect to re-run again until some unrelated future change happens to nudge `target`.
      setDisplay(to);
      return;
    }

    const startTime = performance.now();
    const tick = (now: number) => {
      const t = Math.min((now - startTime) / EASED_DURATION_MS, 1);
      const value = from + (to - from) * easeOutCubic(t);
      currentRef.current = value;
      setDisplay(value);
      frameRef.current = t < 1 ? requestAnimationFrame(tick) : null;
    };
    frameRef.current = requestAnimationFrame(tick);

    return () => {
      if (frameRef.current !== null) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
    };
  }, [target, instant]);

  return instant ? target : display;
}
