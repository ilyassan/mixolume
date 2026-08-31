import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { useSmoothedNumber } from "./useSmoothedNumber";

// Long enough to comfortably outlast the mixer store's own stale-echo protection window
// (`STALE_ECHO_PROTECTION_MS` in `mixer-store.ts`, 400ms) plus real IPC round-trip time, short
// enough that a genuinely external change (auto-duck engaging on this exact session right after
// the user's own commit) isn't hidden for long. See `confirmed`'s own doc comment for why this
// exists at all.
const CONFIRMED_PROTECTION_MS = 500;

// How far (in the same units as the slider's own value, e.g. percent) a track click can drift
// from its own first tick before it's treated as a real drag instead of pointer jitter -- see
// `clickOriginValueRef`'s doc comment for why this exists at all. Comfortably larger than what a
// stationary click's own natural jitter produces (a couple of physical pixels on a real slider
// track, confirmed live to translate to sub-1%-of-range noise even on a narrow control), while
// still small enough that a genuine, deliberate drag clears it within the first couple of real
// pixels of intended movement.
const CLICK_JITTER_THRESHOLD = 3;

/**
 * Tracks a slider's own live drag position, decoupled from whatever asynchronous/round-trip
 * `targetValue` eventually confirms it (a Zustand store update, a backend push, etc).
 *
 * Three things this buys, all confirmed to matter for a fast drag feeling truly smooth, not
 * costing more CPU than it has to, and never flashing a stale value:
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
 * 3. `displayValue` never has to wait on `targetValue` to reflect a value this hook itself just
 *    committed -- see `confirmed`'s own doc comment.
 * 4. Clicking somewhere on the track other than the thumb eases the thumb to that position
 *    instead of jumping there instantly, while grabbing the thumb and actually dragging it still
 *    tracks the pointer 1:1 with no lag -- see `beginDrag`'s and `updateDrag`'s own doc comments.
 */
export function useLiveDragValue(targetValue: number) {
  const [isDragging, setIsDragging] = useState(false);
  // The same fact as `isDragging`, but written synchronously rather than through `setState` --
  // see `isDraggingNow`'s doc comment on the returned object for the confirmed bug this exists
  // to fix. Every place that flips `isDragging` must flip this first, on the same line.
  const isDraggingRef = useRef(false);
  const [liveValue, setLiveValue] = useState<number | null>(null);
  // The latest tick's value, updated synchronously (no re-render) on every call -- `liveValue`
  // (state) can lag this by up to one animation frame while a batched commit is pending.
  const pendingValueRef = useRef<number | null>(null);
  const rafIdRef = useRef<number | null>(null);
  // The most recent value actually committed to `liveValue` during *this* gesture (reset on
  // `beginDrag`) -- see `flushFinalValue`'s doc comment for why this, not just
  // `pendingValueRef`, is needed to get the true final value on release.
  const lastCommittedValueRef = useRef<number | null>(null);
  // True from `beginDrag()` until this gesture's first `updateDrag` tick has committed -- see
  // `updateDrag`'s doc comment for the confirmed bug this exists to fix.
  const isFirstTickRef = useRef(false);
  // Whether the pointer actually grabbed the thumb for *this* gesture (vs. landing elsewhere on
  // the track) -- set from `beginDrag`'s own argument. See `updateDrag`'s doc comment for what
  // this changes about the gesture's first tick.
  const grabbedThumbRef = useRef(true);
  // Whether `liveValue` has actually been used (set to a real value) at any point during *this*
  // gesture -- true immediately for a thumb grab (its first tick always uses it), but only
  // becomes true for a track click if the user keeps moving the pointer past that first tick,
  // turning it into a real drag. `endDragAndCommit` reads this to decide whether release is an
  // instant hand-off (a live value was already on screen, nothing to ease from) or should keep
  // easing (a track click that never became a drag -- see `updateDrag`'s doc comment).
  const usedLiveValueRef = useRef(false);
  // The value a track-click gesture's own first tick landed on, or `null` for a thumb grab (where
  // this distinction doesn't apply) -- lets later ticks in the *same* gesture tell real pointer
  // movement apart from mere jitter around that same spot.
  //
  // Without this, literally any second tick at all -- including one landing within a pixel or two
  // of the first, which real pointer input essentially always produces even for a click a human
  // would say involved no movement at all -- immediately switched the gesture to live tracking
  // (see `updateDrag`'s own doc comment), instantly snapping `displayValue` to wherever that tick
  // landed and abandoning the eased transition `confirmed` was mid-way through. Confirmed live as
  // the cause of exactly the reported symptom: an eased track click "sometimes" cutting off
  // mid-transition and continuing instantly, inconsistently, depending on nothing the user could
  // control -- literally however much sub-pixel noise that specific click's pointer events
  // happened to carry.
  const clickOriginValueRef = useRef<number | null>(null);

  // This hook's own locally-owned "settled" value -- what `displayValue` shows once nothing is
  // actively overriding it (see `usingLiveValue` below), instead of reading `targetValue` (the
  // prop) directly the way earlier versions of this hook did.
  //
  // `displayValue` used to briefly reflect `targetValue` (an *externally* sourced prop,
  // ultimately fed by a store round trip through a backend IPC call and a poll-loop push) instead
  // of what the user actually just set, right after a commit -- visible as the slider snapping
  // back toward its old value and then forward again. `confirmed` sidesteps that class of bug
  // entirely: it's updated *synchronously, locally, the instant a commit happens* (see
  // `endDragAndCommit` and `commitInstant` below), never by waiting for `targetValue` to
  // independently arrive at the same value through a round trip this hook has no control over the
  // timing of.
  //
  // `targetValue` still matters -- it's the only way this hook ever learns about a change that
  // didn't originate from its own caller (auto-duck lowering this session's `effectiveVolume`,
  // most notably) -- but it's only ever adopted into `confirmed` while `protectedUntilRef` says
  // it's safe to (see the reconciliation effect below), not read live on every render.
  const [confirmed, setConfirmed] = useState(targetValue);
  // A `performance.now()` timestamp: reconciling `confirmed` from `targetValue` is suppressed
  // until this passes. Armed for `CONFIRMED_PROTECTION_MS` by every commit (drag-release or
  // instant), specifically so a stale/delayed round trip reflecting *this exact same commit* has
  // no way to reach `confirmed` at all, regardless of how long that round trip takes.
  const protectedUntilRef = useRef(0);

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
  //
  // Separately: a *fast* release commonly fires `pointerup` with no accompanying `onValueChange`
  // tick at all -- the pointer's last real movement already got picked up and committed by a
  // prior animation frame's `updateDrag` call, so by the time `pointerup` arrives there's simply
  // nothing new pending. `pendingValueRef.current` is `null` in that case, which used to make
  // this (and therefore the caller's final commit to the store) return `null` outright -- the
  // caller only commits the released value to the store when this returns non-null, so that
  // session's stored value never updated to reflect the release at all, leaving it frozen at
  // whatever it was *before* the drag started until a much slower backend round-trip eventually
  // corrected it. Visually: the control briefly reverts to its pre-drag position, then jumps to
  // where it was actually released. Confirmed live as the cause of exactly that, specifically on
  // fast drags -- a slow, deliberate drag's last tick tends to still be pending at release.
  // Falling back to `lastCommittedValueRef` (the most recent value this gesture *did* commit,
  // regardless of whether a newer tick was still in flight) fixes this without touching the one
  // case `null` is still correct for: a genuine same-position click-release with no ticks at all.
  const flushFinalValue = useCallback((): number | null => {
    cancelPendingFrame();
    const pending = pendingValueRef.current;
    pendingValueRef.current = null;
    if (pending !== null) {
      lastCommittedValueRef.current = pending;
      flushSync(() => {
        setLiveValue(pending);
      });
      return pending;
    }
    return lastCommittedValueRef.current;
  }, [cancelPendingFrame]);

  // Switches out of dragging mode and, if there's a final value, hands it to `commit`.
  //
  // Also adopts `final` into `confirmed` *synchronously, in this same `flushSync`* -- not by
  // waiting for `targetValue` (the prop, sourced from a Zustand store write read by a *different*
  // component several levels up) to independently arrive at the same value through its own round
  // trip. See `confirmed`'s own doc comment for the class of bug this replaces.
  //
  // Only sets `liveValue` (the instant hand-off) if this gesture actually used it at some point
  // (`usedLiveValueRef` -- see its own doc comment) -- a track click that never turned into a
  // real drag never touched `liveValue` at all, so there's nothing live to hand off; `confirmed`
  // was already set (and is already easing toward it) by `updateDrag`'s own first-tick branch,
  // and setting `liveValue` here too would just override that eased transition with an instant
  // jump right at the very end of it.
  const endDragAndCommit = useCallback(
    (commit: (finalValue: number) => void) => {
      const final = flushFinalValue();
      flushSync(() => {
        isDraggingRef.current = false;
        setIsDragging(false);
        if (final !== null) {
          if (usedLiveValueRef.current) {
            setLiveValue(final);
          }
          setConfirmed(final);
          protectedUntilRef.current = performance.now() + CONFIRMED_PROTECTION_MS;
          commit(final);
        } else {
          // A genuine no-op click-release -- nothing this gesture actually committed, so there's
          // nothing to keep overriding `confirmed` with.
          setLiveValue(null);
        }
      });
    },
    [flushFinalValue],
  );

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
      isDraggingRef.current = false;
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

  // Overriding `confirmed` with `liveValue` isn't only for while `isDragging` is true anymore --
  // see `endDragAndCommit`'s doc comment for why the override now also holds through the (very
  // short, same-commit) gap between a gesture's own commit and `confirmed` reflecting it.
  const usingLiveValue = liveValue !== null;
  // `useSmoothedNumber` returns `target` straight back (synchronously, no extra render) whenever
  // `instant` is true -- see that hook's own doc comment -- so this already *is* `liveValue`
  // while overriding, with no separate fallback needed here.
  //
  // `instant` is `usingLiveValue` alone now, not `isDragging || usingLiveValue` -- a track click
  // (see `updateDrag`'s doc comment) has `isDragging` true for the whole gesture but deliberately
  // never touches `liveValue`, wanting `confirmed` to ease the normal way; OR'ing in `isDragging`
  // forced `instant: true` for that entire gesture regardless, which meant the "eased" value it
  // was easing *to* got displayed the instant it was set -- there was never actually anything on
  // screen left to ease from once `isDragging` finally went false at release. `isDragging` alone
  // (`usingLiveValue` still false) only ever describes the near-zero-duration gap between
  // `beginDrag()` and this same gesture's own first tick, which the first tick's own `flushSync`
  // resolves before anything has a chance to actually paint either way.
  const displayValue = useSmoothedNumber(usingLiveValue ? liveValue : confirmed, usingLiveValue);

  // Releases the override once `confirmed` actually catches up to match it -- an epsilon
  // comparison, not `===`, for the same float round-trip reasons as elsewhere in this file. Only
  // once not dragging: a genuinely new drag tick can legitimately move `liveValue` away from
  // `confirmed` again before this ever gets a chance to run, which is fine -- the next tick's own
  // render just supersedes this effect's stale closure, same as any other effect keyed on a
  // moving value. In practice this now fires on the very next render after every commit, since
  // `endDragAndCommit`/`commitInstant` set `confirmed` to the exact same value in the exact same
  // update as `liveValue` -- there's no external round trip left to wait on.
  useEffect(() => {
    if (!isDragging && liveValue !== null && Math.abs(confirmed - liveValue) < 0.01) {
      setLiveValue(null);
    }
  }, [isDragging, liveValue, confirmed]);

  // Reconciles `confirmed` from `targetValue` (the prop) for changes that didn't originate from
  // this hook's own caller -- most importantly auto-duck lowering `effectiveVolume` out from
  // under an otherwise-idle slider. Suppressed while `protectedUntilRef` says a recent local
  // commit hasn't had time to safely settle yet (see that ref's own doc comment), and never while
  // actively dragging -- a live drag's own `liveValue` override already fully owns `displayValue`
  // regardless of what `confirmed` does underneath it, so there's nothing to protect there and no
  // reason to delay picking up an external change mid-drag. `useLayoutEffect`, not `useEffect`, so
  // any correction this makes resolves before paint -- same reasoning as `useSmoothedNumber`'s and
  // `useSessionListWithFadeOut`'s own identical fix.
  useLayoutEffect(() => {
    if (isDragging) return;
    if (performance.now() < protectedUntilRef.current) return;
    if (confirmed !== targetValue) {
      setConfirmed(targetValue);
    }
  }, [targetValue, isDragging, confirmed]);

  return {
    displayValue,
    isDragging,
    /**
     * Whether a drag is in progress *right now*, readable synchronously -- not the `isDragging`
     * state above, which only becomes true on the next render.
     *
     * Callers must branch on this, not `isDragging`, inside a Radix Slider's `onValueChange`.
     * Confirmed directly (see `VolumeSlider.test.tsx`, which drives the real `<Slider>` with real
     * pointer events): Radix composes the consumer's `onPointerDown` *before* its own internal
     * one, and its internal one synchronously runs `onSlideStart` -> `updateValues` ->
     * `useControllableState`'s setter, which in controlled mode calls `onValueChange` inline. So
     * the whole chain -- `beginDrag()` and the gesture's first `onValueChange` tick -- happens in
     * one event dispatch, with no render in between: a closure reading `isDragging` there still
     * sees `false`. Every track-click and every drag that doesn't start exactly on the thumb was
     * therefore taking the non-drag branch for its first tick, pushing an optimistic store write
     * (plus its own freeze/timer cycle and an unthrottled backend call) into the middle of a
     * gesture that is supposed to stay entirely off that path.
     */
    isDraggingNow: () => isDraggingRef.current,
    /**
     * @param grabbedThumb Whether the pointer actually landed on the thumb itself, as opposed to
     * elsewhere on the track -- see `updateDrag`'s doc comment for what this changes. Callers
     * decide this by checking the pointerdown event's own target, not anything this hook has
     * visibility into.
     */
    beginDrag: (grabbedThumb: boolean) => {
      // Scoped to this gesture -- see `flushFinalValue`'s doc comment.
      lastCommittedValueRef.current = null;
      isFirstTickRef.current = true;
      grabbedThumbRef.current = grabbedThumb;
      usedLiveValueRef.current = false;
      clickOriginValueRef.current = null;
      isDraggingRef.current = true;
      setIsDragging(true);
    },
    /**
     * Call on every `onValueChange` tick while dragging.
     *
     * The gesture's very *first* tick, if the pointer actually grabbed the thumb, is deliberately
     * committed synchronously (via `flushSync`) instead of going through the normal rAF-batched
     * path every later tick uses. Confirmed live, via timestamped diagnostics, as the cause of a
     * real "snaps to the pre-drag value, then corrects" flash on drag start: Radix calls
     * `onValueChange` with this gesture's first value synchronously, inside the very same
     * `onPointerDown` dispatch as `beginDrag()` itself (see `isDraggingNow`'s doc comment) -- but
     * `beginDrag()`'s `setIsDragging(true)` and this tick's `setLiveValue` used to land in two
     * *different* commits (this one deferred to the next animation frame, same as every other
     * tick, for the batching reasons in this hook's own doc comment). That left a real, committed
     * render in between with `isDragging` already `true` (so `useSmoothedNumber` gets
     * `instant: true`) but `liveValue` still `null` -- `usingLiveValue` false, so `displayValue`
     * fell straight back to `confirmed`, i.e. wherever the control was *before* this drag started,
     * for one frame, before the deferred tick corrected it -- on literally every single drag
     * start, not an occasional fluke. This is exactly the mirror image of the hazard
     * `flushFinalValue` already guards against on the opposite (drag-*end*) transition -- this is
     * the drag-*start* one. Only this one tick needs the synchronous treatment; every later tick
     * in the same gesture is between two already-valid, already-painted live positions, where a
     * one-frame lag is not a visible discontinuity, so the batched rAF path (and its real,
     * confirmed CPU savings) stays for those.
     *
     * If the pointer instead landed elsewhere on the track (a "click to jump" gesture, not a
     * thumb grab), the first tick deliberately does the opposite: it eases into the clicked
     * position (via `confirmed`, the same path `commitInstant` uses) instead of jumping there
     * instantly, since there's no live pointer position on the thumb itself to track 1:1 -- a
     * user request, not a bug fix, matching how the thumb already eases toward a value set some
     * other way (auto-duck, a keyboard change).
     *
     * Every tick *after* that first one switches to normal live tracking -- but only once the
     * pointer has actually moved meaningfully from that first tick's own value, not on literally
     * any later tick at all: real pointer input essentially always produces at least one more
     * tick within a pixel or two of the first even for a click a human would say involved no
     * movement whatsoever, and switching to live tracking for *that* meant instantly snapping to
     * wherever that tick landed, cutting off the eased transition `confirmed` was still mid-way
     * through -- confirmed live as the cause of a click's own eased transition "sometimes" cutting
     * off and continuing instantly, depending on nothing the user could control. A tick that's
     * still within `CLICK_JITTER_THRESHOLD` of the click's own origin instead just re-eases toward
     * it (still via `confirmed`, not `liveValue`) -- imperceptible on its own, since the eased
     * transition hasn't gotten far in the handful of milliseconds pointer jitter happens over.
     * Once a tick genuinely clears that threshold, the gesture is unambiguously a real drag from
     * here on, and switches to normal live 1:1 tracking exactly as a thumb grab always has.
     */
    updateDrag: (next: number) => {
      if (isFirstTickRef.current) {
        isFirstTickRef.current = false;
        pendingValueRef.current = null;
        cancelPendingFrame();
        lastCommittedValueRef.current = next;
        if (grabbedThumbRef.current) {
          usedLiveValueRef.current = true;
          flushSync(() => {
            setLiveValue(next);
          });
        } else {
          clickOriginValueRef.current = next;
          protectedUntilRef.current = performance.now() + CONFIRMED_PROTECTION_MS;
          setConfirmed(next);
        }
        return;
      }
      if (
        !usedLiveValueRef.current &&
        clickOriginValueRef.current !== null &&
        Math.abs(next - clickOriginValueRef.current) < CLICK_JITTER_THRESHOLD
      ) {
        // Still just jitter around the same click -- re-ease toward the newer value instead of
        // switching modes. Deliberately not rAF-batched like the live path below: this only ever
        // runs a handful of times per gesture at most (real jitter, not a continuous pointer
        // stream), so there's no CPU-saving reason to defer it.
        lastCommittedValueRef.current = next;
        setConfirmed(next);
        return;
      }
      pendingValueRef.current = next;
      usedLiveValueRef.current = true;
      if (rafIdRef.current === null) {
        rafIdRef.current = requestAnimationFrame(() => {
          rafIdRef.current = null;
          lastCommittedValueRef.current = pendingValueRef.current;
          setLiveValue(pendingValueRef.current);
        });
      }
    },
    /**
     * Call on `onPointerUp`/`onPointerCancel`. `commit` receives the final live value, called
     * synchronously in the same atomic update as switching out of dragging mode -- callers must
     * commit through this, not by calling `endDrag()` and separately doing their own store update
     * with whatever it returns. See `endDragAndCommit`'s own doc comment for the confirmed bug
     * that split-render pattern caused. Not called at all if the drag ended with no ticks (e.g. a
     * same-position click-release).
     */
    endDrag: endDragAndCommit,
    /**
     * Call for any commit that *doesn't* go through a drag gesture at all -- a plain click,
     * a keyboard-driven change, anything that reaches the caller's `onValueChange`'s non-dragging
     * branch. Adopts `value` into `confirmed` immediately and arms the same protection window
     * `endDragAndCommit` does, for the identical reason: displaying `value` shouldn't have to wait
     * on a round trip back through the store to be considered "settled." Doesn't touch
     * `liveValue`/`isDragging` at all -- there's no live gesture here to hand off from, `confirmed`
     * changing is the whole update, and (since `usingLiveValue` is already false here) it eases
     * smoothly into view exactly as before.
     */
    commitInstant: (value: number) => {
      protectedUntilRef.current = performance.now() + CONFIRMED_PROTECTION_MS;
      setConfirmed(value);
    },
  };
}
