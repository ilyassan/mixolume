import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useSessionListWithFadeOut } from "./useSessionListWithFadeOut";
import type { AppSession } from "@/lib/tauri";

const HOLD_MS = 1500;

const makeSession = (overrides: Partial<AppSession> = {}): AppSession => ({
  id: "a",
  displayName: "App A",
  iconPng: null,
  volume: 0.5,
  muted: false,
  isActive: true,
  ...overrides,
});

describe("useSessionListWithFadeOut", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders the initial sessions as not removing", () => {
    const sessions = [makeSession({ id: "a" }), makeSession({ id: "b" })];
    const { result } = renderHook(() =>
      useSessionListWithFadeOut(sessions, HOLD_MS),
    );

    expect(result.current).toHaveLength(2);
    expect(result.current.every((s) => s.removing === false)).toBe(true);
  });

  it("added session case: a newly appearing session is rendered immediately", () => {
    const { result, rerender } = renderHook(
      ({ sessions }) => useSessionListWithFadeOut(sessions, HOLD_MS),
      { initialProps: { sessions: [makeSession({ id: "a" })] } },
    );

    expect(result.current.map((s) => s.id)).toEqual(["a"]);

    act(() => {
      rerender({
        sessions: [makeSession({ id: "a" }), makeSession({ id: "b" })],
      });
    });

    expect(result.current.map((s) => s.id).sort()).toEqual(["a", "b"]);
    expect(result.current.every((s) => s.removing === false)).toBe(true);
  });

  it("removed session case: a session missing from the update fades out, then drops after holdMs", () => {
    const { result, rerender } = renderHook(
      ({ sessions }) => useSessionListWithFadeOut(sessions, HOLD_MS),
      {
        initialProps: {
          sessions: [makeSession({ id: "a" }), makeSession({ id: "b" })],
        },
      },
    );

    // "b" disappears from the backend's list.
    act(() => {
      rerender({ sessions: [makeSession({ id: "a" })] });
    });

    // Still on screen immediately after disappearing, but flagged as removing.
    expect(result.current.map((s) => s.id).sort()).toEqual(["a", "b"]);
    const fading = result.current.find((s) => s.id === "b")!;
    expect(fading.removing).toBe(true);

    // Not dropped yet before the hold elapses.
    act(() => {
      vi.advanceTimersByTime(HOLD_MS - 1);
    });
    expect(result.current.map((s) => s.id)).toContain("b");

    // Dropped once the hold fully elapses.
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.map((s) => s.id)).toEqual(["a"]);
  });

  it("reappearing session case: resumes normally without flicker if it comes back before the fade completes", () => {
    const { result, rerender } = renderHook(
      ({ sessions }) => useSessionListWithFadeOut(sessions, HOLD_MS),
      {
        initialProps: {
          sessions: [makeSession({ id: "a" }), makeSession({ id: "b" })],
        },
      },
    );

    // "b" disappears...
    act(() => {
      rerender({ sessions: [makeSession({ id: "a" })] });
    });
    expect(result.current.find((s) => s.id === "b")!.removing).toBe(true);

    // ...then reappears partway through the hold window.
    act(() => {
      vi.advanceTimersByTime(HOLD_MS / 2);
    });
    act(() => {
      rerender({
        sessions: [
          makeSession({ id: "a" }),
          makeSession({ id: "b", volume: 0.75 }),
        ],
      });
    });

    const resumed = result.current.find((s) => s.id === "b")!;
    expect(resumed.removing).toBe(false);
    expect(resumed.volume).toBe(0.75);

    // Advancing well past the original hold window must NOT remove it - the
    // original removal timer should have been cancelled on reappearance.
    act(() => {
      vi.advanceTimersByTime(HOLD_MS * 2);
    });
    expect(result.current.map((s) => s.id).sort()).toEqual(["a", "b"]);
  });

  it("multiple sessions removed in the same update each fade out independently", () => {
    const { result, rerender } = renderHook(
      ({ sessions }) => useSessionListWithFadeOut(sessions, HOLD_MS),
      {
        initialProps: {
          sessions: [
            makeSession({ id: "a" }),
            makeSession({ id: "b" }),
            makeSession({ id: "c" }),
          ],
        },
      },
    );

    act(() => {
      rerender({ sessions: [makeSession({ id: "a" })] });
    });

    expect(result.current.map((s) => s.id).sort()).toEqual(["a", "b", "c"]);

    act(() => {
      vi.advanceTimersByTime(HOLD_MS);
    });

    expect(result.current.map((s) => s.id)).toEqual(["a"]);
  });
});
