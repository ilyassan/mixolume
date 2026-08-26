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
 * `instant` bypasses the easing (jumps to `target` on the very next animation frame instead of
 * over `EASED_DURATION_MS`) -- pass `true` while the user is actively dragging the control this
 * feeds, so their own input tracks the cursor instead of trailing behind the animation. Still
 * goes through the same `requestAnimationFrame`-driven state update as the eased path rather
 * than a separate synchronous branch: React's lint rules flag `setState` called directly in an
 * effect body, and routing the instant case through one RAF tick satisfies that for free.
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
    }

    const from = currentRef.current;
    const to = target;
    if (from === to) {
      frameRef.current = null;
      return;
    }

    const duration = instant ? 0 : EASED_DURATION_MS;
    const startTime = performance.now();
    const tick = (now: number) => {
      const t = duration === 0 ? 1 : Math.min((now - startTime) / duration, 1);
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

  return display;
}
