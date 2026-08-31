import { describe, it, expect, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useLiveDragValue } from "./useLiveDragValue";

describe("useLiveDragValue", () => {
  it("endDrag commits the last committed value on a fast release with no pending tick", async () => {
    // Regression test: a fast release commonly fires pointerup with no accompanying
    // onValueChange tick -- the pointer's last movement already got committed by a prior
    // animation frame, so there's nothing pending at release time. `endDrag` used to hand back
    // `null` in that case, meaning the caller never told the store about the release at all.
    const { result } = renderHook(() => useLiveDragValue(0));

    act(() => {
      result.current.beginDrag(true);
    });
    act(() => {
      result.current.updateDrag(42);
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });

    const commit = vi.fn();
    act(() => {
      result.current.endDrag(commit);
    });
    expect(commit).toHaveBeenCalledWith(42);
  });

  it("endDrag doesn't commit anything for a genuine same-position click-release with no ticks at all", () => {
    const { result } = renderHook(() => useLiveDragValue(0));

    act(() => {
      result.current.beginDrag(true);
    });

    const commit = vi.fn();
    act(() => {
      result.current.endDrag(commit);
    });
    expect(commit).not.toHaveBeenCalled();
  });

  it("a later drag gesture doesn't see a value committed by an earlier one", async () => {
    const { result } = renderHook(() => useLiveDragValue(0));

    act(() => {
      result.current.beginDrag(true);
    });
    act(() => {
      result.current.updateDrag(10);
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });
    act(() => {
      result.current.endDrag(vi.fn());
    });

    act(() => {
      result.current.beginDrag(true);
    });
    const commit = vi.fn();
    act(() => {
      result.current.endDrag(commit);
    });
    expect(commit).not.toHaveBeenCalled();
  });

  it("endDrag calls commit synchronously and leaves isDragging false afterward", async () => {
    // Regression test: the final commit (e.g. a Zustand store write) used to happen as a plain
    // statement *after* `endDrag()` returned, which could land in a separate render from
    // `isDragging` flipping to `false` -- confirmed live, via render-level timing diagnostics, to
    // produce a render where `isDragging` was already `false` but the caller's own committed
    // value hadn't landed yet, so the displayed target briefly read a stale, unrelated value.
    // `endDrag` now takes the commit itself so both are guaranteed to land in the same render.
    const { result } = renderHook(() => useLiveDragValue(0));

    act(() => {
      result.current.beginDrag(true);
    });
    act(() => {
      result.current.updateDrag(75);
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });

    const commit = vi.fn();
    act(() => {
      result.current.endDrag(commit);
    });
    expect(commit).toHaveBeenCalledWith(75);
    expect(result.current.isDragging).toBe(false);
  });

  it("displayValue never reverts to a stale target while the prop hasn't caught up to a commit yet", async () => {
    // Regression test, the actual reported bug: confirmed live (render-level timing diagnostics)
    // that `targetValue` -- sourced from the mixer store several components up the tree -- does
    // not reliably reflect a gesture's own commit in the very next render, even when the commit
    // and the local `isDragging` flip are bundled into one `flushSync`. This hook must never show
    // that lag: `displayValue` should hold the committed value through it, not fall back to
    // whatever stale `targetValue` still says in the meantime.
    const { result, rerender } = renderHook(
      ({ targetValue }) => useLiveDragValue(targetValue),
      { initialProps: { targetValue: 20 } },
    );

    act(() => {
      result.current.beginDrag(true);
    });
    act(() => {
      result.current.updateDrag(80);
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });
    act(() => {
      result.current.endDrag(vi.fn());
    });
    expect(result.current.displayValue).toBe(80);

    // The prop hasn't caught up yet -- still 20, the pre-drag value -- simulating exactly the
    // lag that was confirmed live. `displayValue` must still read 80, not fall back to 20.
    act(() => {
      rerender({ targetValue: 20 });
    });
    expect(result.current.displayValue).toBe(80);

    // The prop finally catches up to what was committed -- no visible change, just the override
    // quietly releasing.
    act(() => {
      rerender({ targetValue: 80 });
    });
    expect(result.current.displayValue).toBe(80);
  });

  it("a drag gesture's first tick is visible immediately, with no stale in-between frame", () => {
    // Regression test: Radix fires a gesture's first onValueChange tick synchronously, in the
    // same event dispatch as onPointerDown (see isDraggingNow's own doc comment) -- but that
    // first tick used to go through the same requestAnimationFrame-batched path every later tick
    // uses, deferring it by a frame. That left a real, committed render with isDragging already
    // true but liveValue still null, so displayValue fell back to the pre-drag value for one
    // frame before self-correcting. Asserting synchronously, with no rAF flush in between, is the
    // whole point of this test -- if the fix regresses, this reads the stale pre-drag value.
    const { result } = renderHook(() => useLiveDragValue(20));

    act(() => {
      result.current.beginDrag(true);
    });
    act(() => {
      result.current.updateDrag(65);
    });
    expect(result.current.displayValue).toBe(65);
  });

  it("commitInstant settles on its own value, not a stale targetValue, once its ease finishes", () => {
    // Regression test, the non-drag counterpart to the "displayValue never reverts" test above:
    // a plain click or keyboard change doesn't go through beginDrag/endDrag at all, so it still
    // eases smoothly (that part is unchanged, deliberate behavior -- see useSmoothedNumber's own
    // doc comment on `instant`), but the value it eases *toward* must be what was actually
    // committed, not wherever a lagging targetValue prop still says.
    //
    // Fake timers, not a real rAF-flushing loop: the eased transition takes a real 250ms
    // (EASED_DURATION_MS), and racing that against a fixed number of real animation frames is
    // inherently flaky -- how much wall-clock time a given number of frames actually covers isn't
    // guaranteed, especially under load (confirmed flaky exactly this way when run as part of the
    // full suite). Advancing fake time deterministically covers the same ground reliably.
    vi.useFakeTimers();
    try {
      const { result, rerender } = renderHook(
        ({ targetValue }) => useLiveDragValue(targetValue),
        { initialProps: { targetValue: 20 } },
      );

      act(() => {
        result.current.commitInstant(55);
      });

      // The prop hasn't caught up yet -- still 20 -- simulating the store round-trip lag.
      // Without the protection window, this would pull the ease's own target back down to 20.
      act(() => {
        rerender({ targetValue: 20 });
      });

      act(() => {
        vi.advanceTimersByTime(300);
      });

      expect(result.current.displayValue).toBeCloseTo(55, 0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("eases toward a track click (beginDrag(false)) instead of jumping there instantly", () => {
    // User-requested behavior, not a bug fix: clicking somewhere on the track other than the
    // thumb should feel like a deliberate, smooth move to that position, matching how a
    // programmatic change (auto-duck, commitInstant) already eases -- not the instant 1:1
    // hand-off a real thumb grab uses, since there's no live pointer position on the thumb to
    // track in that case.
    vi.useFakeTimers();
    try {
      const { result } = renderHook(() => useLiveDragValue(20));

      act(() => {
        result.current.beginDrag(false);
      });
      act(() => {
        result.current.updateDrag(80);
      });
      // Immediately after the single tick, still mid-transition -- not yet at 80.
      expect(result.current.displayValue).not.toBe(80);

      const commit = vi.fn();
      act(() => {
        result.current.endDrag(commit);
      });
      expect(commit).toHaveBeenCalledWith(80);

      act(() => {
        vi.advanceTimersByTime(300);
      });
      expect(result.current.displayValue).toBeCloseTo(80, 0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("switches to live 1:1 tracking if a track click turns into a real drag", async () => {
    // The pointer genuinely moving after the first tick means the gesture became a real drag
    // regardless of how it started -- from there it must track 1:1 like any other drag, not
    // keep easing.
    const { result } = renderHook(() => useLiveDragValue(20));

    act(() => {
      result.current.beginDrag(false);
    });
    act(() => {
      result.current.updateDrag(50);
    });
    act(() => {
      result.current.updateDrag(80);
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });
    expect(result.current.displayValue).toBe(80);
  });

  it("keeps easing through a click's own pointer jitter, instead of cutting to instant", () => {
    // Regression test, the actual reported bug: a click's pointerdown/pointerup pair essentially
    // always carries at least one more tick within a pixel or two of the first, even when a human
    // would say no movement happened at all. Before this, *any* second tick switched the gesture
    // to live tracking, snapping instantly to that tick's value and abandoning the eased
    // transition -- inconsistently, depending on nothing the user could control (however much
    // sub-pixel noise that specific click's pointer events happened to carry). A tick within
    // `CLICK_JITTER_THRESHOLD` of the click's own first value must keep easing, not switch modes.
    vi.useFakeTimers();
    try {
      const { result } = renderHook(() => useLiveDragValue(20));

      act(() => {
        result.current.beginDrag(false);
      });
      act(() => {
        result.current.updateDrag(80);
      });
      // A little jitter around the same click -- well under the threshold.
      act(() => {
        result.current.updateDrag(80.5);
      });
      act(() => {
        result.current.updateDrag(79.7);
      });
      // Still mid-ease, not snapped instantly to any of those.
      expect(result.current.displayValue).not.toBe(80.5);
      expect(result.current.displayValue).not.toBe(79.7);

      const commit = vi.fn();
      act(() => {
        result.current.endDrag(commit);
      });
      expect(commit).toHaveBeenCalledWith(79.7);

      act(() => {
        vi.advanceTimersByTime(300);
      });
      expect(result.current.displayValue).toBeCloseTo(79.7, 0);
    } finally {
      vi.useRealTimers();
    }
  });
});
