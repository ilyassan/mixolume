import { useEffect, useRef, useState } from "react";
import type { AppSession } from "@/lib/tauri";

export interface FadingSession extends AppSession {
  /** True once the session has disappeared from the backend's list and is
   * being held on screen for a fade-out transition before it's dropped. */
  removing: boolean;
}

/**
 * Renders the given `sessions` list, but instead of instantly unmounting a
 * session the moment it disappears from an update, holds it in local state
 * (flagged `removing: true`) for `holdMs` so the caller can play a fade-out
 * transition, then drops it.
 *
 * If a session reappears before its hold timer fires, it simply resumes as a
 * normal (non-removing) entry - no flicker, no early removal.
 */
export function useSessionListWithFadeOut(
  sessions: AppSession[],
  holdMs: number,
): FadingSession[] {
  const [rendered, setRendered] = useState<FadingSession[]>(() =>
    sessions.map((session) => ({ ...session, removing: false })),
  );
  const timersRef = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  useEffect(() => {
    setRendered((prev) => {
      const nextIds = new Set(sessions.map((session) => session.id));

      // Present in the new list: render fresh data, not removing. Also
      // cancel any pending removal timer - the session reappeared.
      const current: FadingSession[] = sessions.map((session) => {
        const pendingTimer = timersRef.current.get(session.id);
        if (pendingTimer) {
          clearTimeout(pendingTimer);
          timersRef.current.delete(session.id);
        }
        return { ...session, removing: false };
      });

      // Was rendered before but is missing from the new list: keep its last
      // known data on screen, flagged as removing, and (if not already
      // scheduled) start the hold timer that will drop it for good.
      const stillFading: FadingSession[] = [];
      for (const prevSession of prev) {
        if (nextIds.has(prevSession.id)) {
          continue;
        }
        if (!timersRef.current.has(prevSession.id)) {
          const timer = setTimeout(() => {
            setRendered((latest) =>
              latest.filter((session) => session.id !== prevSession.id),
            );
            timersRef.current.delete(prevSession.id);
          }, holdMs);
          timersRef.current.set(prevSession.id, timer);
        }
        stillFading.push({ ...prevSession, removing: true });
      }

      return [...current, ...stillFading];
    });
  }, [sessions, holdMs]);

  // Clear any outstanding timers on unmount.
  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      for (const timer of timers.values()) {
        clearTimeout(timer);
      }
      timers.clear();
    };
  }, []);

  return rendered;
}
