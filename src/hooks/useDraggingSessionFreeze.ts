import { useEffect, useRef } from "react";
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
 */
export function useDraggingSessionFreeze(sessionId: string, isDragging: boolean) {
  const setDraggingSessionId = useMixerStore((state) => state.setDraggingSessionId);

  useEffect(() => {
    if (isDragging) {
      setDraggingSessionId(sessionId);
      return;
    }
    const timeoutId = setTimeout(() => {
      setDraggingSessionId(null);
    }, RELEASE_GRACE_PERIOD_MS);
    return () => clearTimeout(timeoutId);
  }, [isDragging, sessionId, setDraggingSessionId]);

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
      if (isDraggingRef.current) {
        setDraggingSessionId(null);
      }
    };
  }, [setDraggingSessionId]);
}
