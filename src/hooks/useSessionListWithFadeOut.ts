import { useEffect, useRef, useState } from "react";
import type { AppSession } from "@/lib/tauri";

export interface FadingSession extends AppSession {
  /** True once the session has disappeared from the backend's list and is
   * being held on screen for a fade-out transition before it's dropped. */
  removing: boolean;
}

/**
 * Renders the given `sessions` list, but smooths out two kinds of backend
 * flicker instead of reflecting them instantly:
 *
 * - A session disappearing from the list entirely (its process stopped
 *   producing any Core Audio output object at all) is held in local state
 *   (flagged `removing: true`) for `holdMs` so the caller can play a
 *   fade-out transition, then dropped.
 * - A session staying in the list but its `isActive` flag flipping to
 *   `false` is *displayed* as still active for `holdMs` after the last time
 *   it was confirmed active. `kAudioProcessPropertyIsRunningOutput` (the
 *   source of `isActive`) can report a brief false reading for a genuinely
 *   still-playing app -- e.g. a buffering gap between tracks -- and without
 *   this hold, that one 700ms poll tick would visibly jump the row to the
 *   "Inactive" section and back, which is exactly what it looks like from
 *   the outside: an app that's clearly making sound shown as inactive.
 *
 * If a session reappears (or goes active again) before its hold timer
 * fires, it simply resumes as normal - no flicker, no early transition.
 */
export function useSessionListWithFadeOut(
  sessions: AppSession[],
  holdMs: number,
): FadingSession[] {
  const [rendered, setRendered] = useState<FadingSession[]>(() =>
    sessions.map((session) => ({ ...session, removing: false })),
  );
  const removalTimersRef = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  // Wall-clock time each session id was last reported `isActive: true`. A
  // deactivation timer (below) flips `rendered` directly once a session's
  // hold window closes, so a stale reading still turns inactive on time even
  // if no further backend update ever arrives for it.
  const lastActiveAtRef = useRef(new Map<string, number>());
  const deactivationTimersRef = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  useEffect(() => {
    const now = Date.now();
    const seenIds = new Set(sessions.map((session) => session.id));

    for (const session of sessions) {
      if (session.isActive) {
        lastActiveAtRef.current.set(session.id, now);
        const pending = deactivationTimersRef.current.get(session.id);
        if (pending) {
          clearTimeout(pending);
          deactivationTimersRef.current.delete(session.id);
        }
        continue;
      }
      // Reported inactive, but still present in the list: if we're not
      // already holding it, schedule a re-render for when its hold window
      // closes (so it flips over on time even without fresh data).
      const lastActiveAt = lastActiveAtRef.current.get(session.id);
      const withinHold = lastActiveAt !== undefined && now - lastActiveAt < holdMs;
      if (withinHold && !deactivationTimersRef.current.has(session.id)) {
        const timer = setTimeout(() => {
          deactivationTimersRef.current.delete(session.id);
          setRendered((prev) =>
            prev.map((s) =>
              s.id === session.id ? { ...s, isActive: false } : s,
            ),
          );
        }, lastActiveAt! + holdMs - Date.now());
        deactivationTimersRef.current.set(session.id, timer);
      }
    }

    // Drop hold-tracking state for ids that are no longer coming from the
    // backend at all -- the separate removal/fade-out handling below covers
    // their on-screen lifetime.
    for (const id of lastActiveAtRef.current.keys()) {
      if (!seenIds.has(id)) {
        lastActiveAtRef.current.delete(id);
      }
    }
    for (const [id, timer] of deactivationTimersRef.current) {
      if (!seenIds.has(id)) {
        clearTimeout(timer);
        deactivationTimersRef.current.delete(id);
      }
    }

    setRendered((prev) => {
      const nextIds = seenIds;

      // Present in the new list: render fresh data, not removing. Also
      // cancel any pending removal timer - the session reappeared.
      const current: FadingSession[] = sessions.map((session) => {
        const pendingTimer = removalTimersRef.current.get(session.id);
        if (pendingTimer) {
          clearTimeout(pendingTimer);
          removalTimersRef.current.delete(session.id);
        }
        if (session.isActive) {
          return { ...session, removing: false };
        }
        const lastActiveAt = lastActiveAtRef.current.get(session.id);
        const stillHeldActive =
          lastActiveAt !== undefined && now - lastActiveAt < holdMs;
        return { ...session, isActive: stillHeldActive, removing: false };
      });

      // Was rendered before but is missing from the new list: keep its last
      // known data on screen, flagged as removing, and (if not already
      // scheduled) start the hold timer that will drop it for good.
      const stillFading: FadingSession[] = [];
      for (const prevSession of prev) {
        if (nextIds.has(prevSession.id)) {
          continue;
        }
        if (!removalTimersRef.current.has(prevSession.id)) {
          const timer = setTimeout(() => {
            setRendered((latest) =>
              latest.filter((session) => session.id !== prevSession.id),
            );
            removalTimersRef.current.delete(prevSession.id);
          }, holdMs);
          removalTimersRef.current.set(prevSession.id, timer);
        }
        stillFading.push({ ...prevSession, removing: true });
      }

      return [...current, ...stillFading];
    });
  }, [sessions, holdMs]);

  // Clear any outstanding timers on unmount.
  useEffect(() => {
    const removalTimers = removalTimersRef.current;
    const deactivationTimers = deactivationTimersRef.current;
    return () => {
      for (const timer of removalTimers.values()) {
        clearTimeout(timer);
      }
      removalTimers.clear();
      for (const timer of deactivationTimers.values()) {
        clearTimeout(timer);
      }
      deactivationTimers.clear();
    };
  }, []);

  return rendered;
}
