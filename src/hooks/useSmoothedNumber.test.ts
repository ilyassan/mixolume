import { describe, it, expect } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useSmoothedNumber } from "./useSmoothedNumber";

describe("useSmoothedNumber", () => {
  it("instant mode returns the target directly and immediately", () => {
    const { result } = renderHook(
      ({ target, instant }) => useSmoothedNumber(target, instant),
      { initialProps: { target: 50, instant: true } },
    );
    expect(result.current).toBe(50);
  });

  it("instant mode tracks a changing target on every render, with no lag", () => {
    const { result, rerender } = renderHook(
      ({ target, instant }) => useSmoothedNumber(target, instant),
      { initialProps: { target: 10, instant: true } },
    );

    act(() => {
      rerender({ target: 42.7, instant: true });
    });
    expect(result.current).toBe(42.7);

    act(() => {
      rerender({ target: 91.3, instant: true });
    });
    expect(result.current).toBe(91.3);
  });

  it("landing back on non-instant mode at the exact value already reached during instant mode does not snap to a stale earlier value", () => {
    // Regression test: a drag release commonly lands exactly on the value already tracked
    // internally from the drag itself (that's the point of keeping it in sync throughout) --
    // this used to leave the hook returning whatever it had rendered *before* the drag ever
    // started, because the "nothing to animate" early-return path never synced `display`.
    const { result, rerender } = renderHook(
      ({ target, instant }) => useSmoothedNumber(target, instant),
      { initialProps: { target: 20, instant: false } },
    );
    expect(result.current).toBe(20);

    // Simulate a drag: instant mode tracks a live value that ends up at 73.42.
    act(() => {
      rerender({ target: 73.42, instant: true });
    });
    expect(result.current).toBe(73.42);

    // Drag ends -- the store's optimistic update lands on that exact same value, so there's
    // nothing to actually animate.
    act(() => {
      rerender({ target: 73.42, instant: false });
    });
    expect(result.current).toBe(73.42);
  });

  it("a genuine value change in non-instant mode eases to the target rather than jumping instantly", async () => {
    const { result, rerender } = renderHook(
      ({ target, instant }) => useSmoothedNumber(target, instant),
      { initialProps: { target: 0, instant: false } },
    );
    expect(result.current).toBe(0);

    act(() => {
      rerender({ target: 100, instant: false });
    });
    // The very first render after a real (non-instant) target change hasn't had a chance to
    // run any animation frame yet -- still 0, not jumped straight to 100.
    expect(result.current).toBe(0);

    // Give the RAF-driven ease plenty of real time to fully finish and confirm it actually
    // converges on the target -- proves this path still animates rather than being skipped
    // somehow. Generous margin (well beyond EASED_DURATION_MS) because jsdom's `requestAnimationFrame`
    // polyfill doesn't tick at a precise real-time rate, so this only asserts eventual
    // convergence, not timing.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 3000));
    });
    expect(result.current).toBe(100);
  });
});
