import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { VolumeSlider } from "./VolumeSlider";

// The store (reached via `useDraggingSessionFreeze`) and the throttled drag path both talk to
// Tauri; nothing here needs a real backend, only a record of what was called.
const invokeMock = vi.fn<(command: string, args?: unknown) => Promise<void>>(() =>
  Promise.resolve(),
);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => invokeMock(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => {}),
}));

// Radix's thumb measures itself with a ResizeObserver, which jsdom doesn't implement.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

// Radix maps pointer position to a value off the root's own rect, which is all zeroes in jsdom
// (making every computed value `NaN`), and drives the gesture through pointer capture, which jsdom
// doesn't implement either. With these, a real `<Slider>` responds to real pointer events: clientX
// 0 -> 0%, clientX 200 -> 100%.
const SLIDER_WIDTH = 200;

function stubSliderGeometry() {
  Element.prototype.getBoundingClientRect = () =>
    ({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: SLIDER_WIDTH,
      bottom: 10,
      width: SLIDER_WIDTH,
      height: 10,
      toJSON: () => ({}),
    }) as DOMRect;
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => true;
}

function nextFrame() {
  return act(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
  });
}

function drainThrottleQueue() {
  // `useThrottledCallback` defers the actual call with `setTimeout(..., 0)`.
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function renderSlider(
  onVolumeChange: (sessionId: string, volume: number) => void,
  onMute: () => void = () => {},
  onUnmute: () => void = () => {},
) {
  render(
    <VolumeSlider
      sessionId="s1"
      displayName="App"
      percent={20}
      maxVolumePercent={100}
      disabled={false}
      muted={false}
      onVolumeChange={onVolumeChange}
      onUnmute={onUnmute}
      onMute={onMute}
    />,
  );
  return screen.getByRole("slider").closest("[data-slot='slider']")!;
}

const volumeCommands = () =>
  invokeMock.mock.calls.filter(([command]) => command === "set_volume").map(([, args]) => args);

describe("VolumeSlider", () => {
  const originalRect = Element.prototype.getBoundingClientRect;

  beforeEach(() => {
    invokeMock.mockClear();
    stubSliderGeometry();
  });

  afterEach(() => {
    Element.prototype.getBoundingClientRect = originalRect;
  });

  it("keeps the whole pointer gesture off the store's setVolume action, first tick included", async () => {
    // Regression test for the actual cause of the reported slider flicker. Radix composes the
    // consumer's `onPointerDown` before its own, and its own synchronously produces the gesture's
    // first `onValueChange` -- so that tick runs before React has re-rendered with the
    // `beginDrag()` that just happened, and a branch reading the `isDragging` *state* saw `false`
    // and sent the tick through the store: an optimistic update plus a freeze/timer cycle plus an
    // unthrottled backend write, in the middle of a gesture that is supposed to stay entirely on
    // the throttled path until it ends. Confirmed by this test failing on the pre-fix code with
    // `onVolumeChange` called on pointerdown.
    const onVolumeChange = vi.fn();
    const slider = renderSlider(onVolumeChange);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    expect(onVolumeChange).not.toHaveBeenCalled();

    await nextFrame();
    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 120 });
    await nextFrame();
    expect(onVolumeChange).not.toHaveBeenCalled();

    // ...but the backend is still driven directly (throttled) throughout, so audio tracks the drag.
    await drainThrottleQueue();
    expect(volumeCommands()).toContainEqual({ sessionId: "s1", volume: 0.8 });
  });

  it("commits the released value to the store once, when the gesture ends", async () => {
    const onVolumeChange = vi.fn();
    const slider = renderSlider(onVolumeChange);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    await nextFrame();
    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 120 });
    await nextFrame();
    fireEvent.pointerUp(slider, { pointerId: 1, clientX: 120 });

    expect(onVolumeChange.mock.calls).toEqual([["s1", 0.6]]);
  });

  it("commits a plain track click, which has no pointermove ticks at all", () => {
    // The pointerdown tick is now the gesture's only value tick, and it lands in the drag path
    // (`updateDrag`), so the release has to be what carries it to the store. Before the fix this
    // worked only because the pointerdown tick went to the store directly.
    const onVolumeChange = vi.fn();
    const slider = renderSlider(onVolumeChange);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    fireEvent.pointerUp(slider, { pointerId: 1, clientX: 160 });

    expect(onVolumeChange.mock.calls).toEqual([["s1", 0.8]]);
  });

  it("still routes a keyboard change through the store", () => {
    const onVolumeChange = vi.fn();
    renderSlider(onVolumeChange);

    fireEvent.keyDown(screen.getByRole("slider"), { key: "ArrowRight" });

    expect(onVolumeChange.mock.calls).toEqual([["s1", 0.201]]);
  });

  it("mutes when a drag is released at 0%", async () => {
    const onVolumeChange = vi.fn();
    const onMute = vi.fn();
    const slider = renderSlider(onVolumeChange, onMute);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    await nextFrame();
    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 0 });
    await nextFrame();
    fireEvent.pointerUp(slider, { pointerId: 1, clientX: 0 });

    expect(onVolumeChange.mock.calls).toEqual([["s1", 0]]);
    expect(onMute).toHaveBeenCalledOnce();
  });

  it("mutes live, the moment a drag reaches 0%, without waiting for the release", async () => {
    // The actual reported gap this fixes: muting only ever fired on `endDrag`'s commit, so the
    // mute button's own icon stayed un-muted for the whole drag down to 0%, only flipping the
    // instant the pointer lifted -- not live, as the user's own volume actually reads 0% (and
    // therefore is already silent) well before that.
    const onVolumeChange = vi.fn();
    const onMute = vi.fn();
    const slider = renderSlider(onVolumeChange, onMute);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    await nextFrame();
    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 0 });
    await nextFrame();

    // Still mid-drag -- no pointerUp yet -- but onMute must have already fired.
    expect(onMute).toHaveBeenCalledOnce();
  });

  it("unmutes live if a drag moves back above 0% within the same gesture", async () => {
    const onVolumeChange = vi.fn();
    const onMute = vi.fn();
    const onUnmute = vi.fn();
    const slider = renderSlider(onVolumeChange, onMute, onUnmute);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    await nextFrame();
    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 0 });
    await nextFrame();
    expect(onMute).toHaveBeenCalledOnce();

    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 120 });
    await nextFrame();
    expect(onUnmute).toHaveBeenCalledOnce();

    fireEvent.pointerUp(slider, { pointerId: 1, clientX: 120 });
    expect(onVolumeChange.mock.calls).toEqual([["s1", 0.6]]);
    // Already handled live -- the release commit must not fire it a second time.
    expect(onMute).toHaveBeenCalledOnce();
  });

  it("mutes on a plain click at 0%", () => {
    const onVolumeChange = vi.fn();
    const onMute = vi.fn();
    const slider = renderSlider(onVolumeChange, onMute);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 0 });
    fireEvent.pointerUp(slider, { pointerId: 1, clientX: 0 });

    expect(onVolumeChange.mock.calls).toEqual([["s1", 0]]);
    expect(onMute).toHaveBeenCalledOnce();
  });

  it("does not mute when a drag settles above 0%", async () => {
    const onVolumeChange = vi.fn();
    const onMute = vi.fn();
    const slider = renderSlider(onVolumeChange, onMute);

    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    await nextFrame();
    fireEvent.pointerMove(slider, { pointerId: 1, clientX: 120 });
    await nextFrame();
    fireEvent.pointerUp(slider, { pointerId: 1, clientX: 120 });

    expect(onVolumeChange.mock.calls).toEqual([["s1", 0.6]]);
    expect(onMute).not.toHaveBeenCalled();
  });

  it("grabbing the thumb directly tracks the pointer instantly, with no easing", () => {
    // The component-level counterpart to useLiveDragValue.test.ts's own coverage of this --
    // exercises the actual DOM-based "did the pointer land on the thumb" detection
    // (`event.target.closest('[data-slot="slider-thumb"]')` in VolumeSlider.tsx) rather than
    // calling `beginDrag(true)` directly, since that detection is the one piece of this behavior
    // the hook-level tests can't see at all.
    const onVolumeChange = vi.fn();
    renderSlider(onVolumeChange);
    const thumb = screen.getByRole("slider");

    // Radix doesn't recompute a value from pointer position for a thumb grab -- it's already at
    // its own value, so pointerdown right on it (its actual current position, 20% of 200px = 40)
    // produces no tick at all; only the pointermove that follows does.
    fireEvent.pointerDown(thumb, { pointerId: 1, clientX: 40 });
    fireEvent.pointerMove(thumb, { pointerId: 1, clientX: 160 });
    // Same tick, no rAF flush -- a real thumb grab shows the new value immediately.
    expect(thumb.getAttribute("aria-valuenow")).toBe("80");
  });

  it("clicking the track away from the thumb doesn't jump there instantly", () => {
    const onVolumeChange = vi.fn();
    const slider = renderSlider(onVolumeChange);
    const thumb = screen.getByRole("slider");

    // Fired on the root, not the thumb -- a genuine track click. Starting value is 20% (see
    // `renderSlider`), so a click at clientX 160 (80%) is nowhere near the thumb's own position.
    fireEvent.pointerDown(slider, { pointerId: 1, clientX: 160 });
    // Unlike the thumb-grab case above, this must NOT already read the clicked value -- it's
    // still mid-ease.
    expect(thumb.getAttribute("aria-valuenow")).not.toBe("80");
  });
});
