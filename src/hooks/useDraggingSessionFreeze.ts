import { useCallback, useEffect, useRef } from "react";
import { useMixerStore } from "@/stores/mixer-store";

// Long enough to guarantee the backend's own poll loop (up to ~150ms between ticks) gets at
// least one full cycle to read the truly-final value after the drag's last command was sent,
// before this session stops being protected from a stale push overwriting it. See this hook's
// own doc comment for why releasing the freeze the instant a drag ends isn't actually safe.
const RELEASE_GRACE_PERIOD_MS = 400;

/**
 * Keeps the mixer store's `draggingSessionId` freeze (see that field's own doc comment) active
 * for `sessionId` while `isDragging` is true, *and* for a short grace period after it goes false.
 *
 * The grace period exists because releasing the freeze the instant `isDragging` flips isn't
 * actually safe: the backend's own poll loop runs on its own independent schedule the whole
 * time a drag is happening, and can have a poll already in flight that read the volume *before*
 * the drag's final command was applied server-side. Without the grace period, that stale read
 * arrives right after the freeze lifts and briefly overwrites the just-released value -- visible
 * as the slider snapping away from where it was let go and then snapping back once a later,
 * correct poll corrects it. Confirmed live as the cause of exactly that symptom.
 *
 * Shared by `VolumeSlider` and `BalanceSliders` rather than duplicated in both.
 *
 * Engaging the freeze is exposed as `beginFreeze()`, a plain synchronous function the caller must
 * invoke directly from the same `onPointerDown` handler that starts the drag -- NOT done
 * automatically here via a `useEffect` watching `isDragging`. That used to be exactly how this
 * worked, and it was a real, confirmed bug: `useEffect` only runs *after* a render has committed
 * and the browser has painted it. `beginDrag()` (in `useLiveDragValue`) flips `isDraggingRef`
 * synchronously, but flips `isDragging` *state* through `setIsDragging`, which needs a render to
 * take effect before this hook's effect -- watching that same state -- can even run. That leaves a
 * real window, right at the start of every single drag (not a rare race), between "the user started
 * dragging" and "the store's `draggingSessionId` freeze actually engages," during which the
 * throttled backend call from the drag's very first tick can round-trip through the backend's own
 * poll loop and arrive back as a push with nothing there yet to buffer it -- applied directly to
 * `state.sessions`, exactly the un-buffered write this freeze exists to prevent. Confirmed live via
 * an isolation ladder that reproduced the flicker through the real store with neither Framer Motion
 * nor `SessionRow` involved at all, which narrowed it to here: the store round trip itself, not
 * rendering, animation, or the store's other logic (`mergeSessions`, `protectFromStaleEcho`, etc.,
 * all separately ruled out). Calling `beginFreeze()` synchronously, in the same event dispatch as
 * `beginDrag()`, closes that window entirely -- the freeze is live in the store before any tick's
 * backend call can possibly round-trip back.
 */
export function useDraggingSessionFreeze(sessionId: string, isDragging: boolean) {
  const setDraggingSessionId = useMixerStore((state) => state.setDraggingSessionId);
  const endFreezeIfCurrent = useMixerStore((state) => state.endFreezeIfCurrent);

  const beginFreeze = useCallback(() => {
    setDraggingSessionId(sessionId);
  }, [setDraggingSessionId, sessionId]);

  useEffect(() => {
    if (isDragging) {
      // Safety net only, not the primary path -- normally already engaged synchronously by
      // `beginFreeze()` at the exact moment the drag started (see this hook's own doc comment).
      // This just covers `isDragging` becoming true through some other route that didn't call
      // `beginFreeze` itself (e.g. the window-level pointer backstop re-arming). Idempotent if the
      // freeze is already engaged for this session: re-arming just bumps the generation counter,
      // which is harmless -- `endFreezeIfCurrent` only ever compares it to whatever's current.
      setDraggingSessionId(sessionId);
      return;
    }
    // Captures *this* gesture's generation (see `draggingGeneration`'s doc comment) right after
    // arming, so the release below only fires if nothing re-armed the freeze in between -- not a
    // plain `setDraggingSessionId(null)`. Both this timer and `protectFromStaleEcho` (in the
    // store) arm the same single `draggingSessionId` field, and a rapid sequence of separate
    // click/drag gestures on the same session (a real pattern, not hypothetical) can easily have
    // one gesture's grace-period timer still pending when a *later* gesture re-arms the freeze
    // for the same session id. A session-id-only guard can't tell those apart -- confirmed live,
    // via timing diagnostics, as a real bug: one gesture's timer fired ~24ms after its freeze was
    // set, nowhere near the intended 400ms, because the timer that actually fired belonged to an
    // earlier gesture whose own grace period happened to elapse right then. The result: a stale
    // backend push landing in that accidentally-shortened window applied immediately instead of
    // being buffered -- the actual root cause of the reported flicker.
    const generation = useMixerStore.getState().draggingGeneration;
    const timeoutId = setTimeout(() => {
      endFreezeIfCurrent(sessionId, generation);
    }, RELEASE_GRACE_PERIOD_MS);
    return () => clearTimeout(timeoutId);
  }, [isDragging, sessionId, setDraggingSessionId, endFreezeIfCurrent]);

  // Keeps a ref in sync with the latest `isDragging`, purely so the unmount-only effect below can
  // read it without needing `isDragging` in *its own* deps -- which would make it re-run (and its
  // cleanup fire) on every drag start/stop, not only on unmount; see that effect's own comment.
  const isDraggingRef = useRef(isDragging);
  useEffect(() => {
    isDraggingRef.current = isDragging;
  }, [isDragging]);

  // Unmount-only safety net, deliberately separate from the effect above: if this component
  // disappears while still actively dragging (e.g. the session vanished mid-drag), clear the
  // freeze immediately rather than leaving it stuck pointing at a session that's gone. A real
  // unmount is the only case that needs to skip the grace period -- an ordinary re-render where
  // `isDragging` merely flips to false must still go through it, which is why this can't just be
  // folded into the first effect's cleanup (that cleanup also fires on every dependency change,
  // not only on unmount, and would defeat the grace period on every normal drag release).
  // `setDraggingSessionId` is a stable Zustand action reference, so this effect's identity never
  // actually changes across renders -- its cleanup only ever really fires on unmount in practice.
  useEffect(() => {
    return () => {
      // Also guarded (see the effect above) -- if something else already took over the freeze
      // by the time this unmounts, clearing unconditionally would release *that* one instead.
      if (isDraggingRef.current && useMixerStore.getState().draggingSessionId === sessionId) {
        setDraggingSessionId(null);
      }
    };
  }, [setDraggingSessionId, sessionId]);

  return { beginFreeze };
}
