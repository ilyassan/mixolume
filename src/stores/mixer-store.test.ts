import { describe, it, expect, vi, beforeEach } from "vitest";

const {
  listSessions,
  setVolume,
  setMuted,
  setBalance,
  maxVolumePercent,
  outputRoutingSupported,
  listOutputDevices,
  setSessionOutputDevice,
  duckingSupported,
  getDuckingSettings,
  setDuckingEnabled,
  setDuckTriggerPriority,
  capturedCallback,
  unlistenMock,
} = vi.hoisted(() => {
  const unlistenMock = vi.fn();
  return {
    listSessions: vi.fn(),
    setVolume: vi.fn(),
    setMuted: vi.fn(),
    setBalance: vi.fn(),
    maxVolumePercent: vi.fn(),
    // Defaults to unsupported, matching every backend that doesn't override
    // `output_routing_supported` -- tests that specifically exercise output routing set this to
    // resolve `true` themselves.
    outputRoutingSupported: vi.fn().mockResolvedValue(false),
    listOutputDevices: vi.fn().mockResolvedValue([]),
    setSessionOutputDevice: vi.fn(),
    // Same "defaults to unsupported" reasoning as `outputRoutingSupported` above.
    duckingSupported: vi.fn().mockResolvedValue(false),
    getDuckingSettings: vi.fn().mockResolvedValue({
      enabled: false,
      priorityTriggers: [],
      priorityTriggerIcons: {},
    }),
    setDuckingEnabled: vi.fn(),
    setDuckTriggerPriority: vi.fn(),
    capturedCallback: { current: null as null | ((s: unknown[]) => void) },
    unlistenMock,
  };
});

vi.mock("@/lib/tauri", () => ({
  listSessions: (...args: unknown[]) => listSessions(...args),
  setVolume: (...args: unknown[]) => setVolume(...args),
  setMuted: (...args: unknown[]) => setMuted(...args),
  setBalance: (...args: unknown[]) => setBalance(...args),
  maxVolumePercent: (...args: unknown[]) => maxVolumePercent(...args),
  outputRoutingSupported: (...args: unknown[]) => outputRoutingSupported(...args),
  listOutputDevices: (...args: unknown[]) => listOutputDevices(...args),
  setSessionOutputDevice: (...args: unknown[]) => setSessionOutputDevice(...args),
  duckingSupported: (...args: unknown[]) => duckingSupported(...args),
  getDuckingSettings: (...args: unknown[]) => getDuckingSettings(...args),
  setDuckingEnabled: (...args: unknown[]) => setDuckingEnabled(...args),
  setDuckTriggerPriority: (...args: unknown[]) => setDuckTriggerPriority(...args),
  listenToSessionsChanged: vi.fn((callback: (s: unknown[]) => void) => {
    capturedCallback.current = callback;
    return Promise.resolve(unlistenMock);
  }),
  // Real implementation, not a mock -- it's a pure string check, and tests
  // below rely on it actually matching the real error format.
  isPermissionError: (error: unknown) =>
    typeof error === "string" &&
    error.includes("screen & system audio recording permission"),
}));

import { useMixerStore, __resetWriteGenerationsForTests } from "./mixer-store";
import type { AppSession } from "@/lib/tauri";

const session = (overrides: Partial<AppSession> = {}): AppSession => ({
  id: "session-1",
  displayName: "Test App",
  iconPng: null,
  volume: 0.5,
  effectiveVolume: 0.5,
  muted: false,
  balance: 0,
  isActive: true,
  isDuckTrigger: false,
  isDucked: false,
  writeGeneration: 0,
  outputDeviceId: null,
  ...overrides,
});

describe("mixer-store", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    __resetWriteGenerationsForTests();
    capturedCallback.current = null;
    listSessions.mockResolvedValue([]);
    setVolume.mockResolvedValue(1);
    setMuted.mockResolvedValue(1);
    setBalance.mockResolvedValue(1);
    maxVolumePercent.mockResolvedValue(100);
    useMixerStore.setState({
      sessions: [],
      isLoaded: false,
      isInitialized: false,
      needsPermission: false,
      maxVolumePercent: 100,
      draggingSessionId: null,
    });
  });

  it("starts unloaded with no sessions", () => {
    const state = useMixerStore.getState();
    expect(state.isLoaded).toBe(false);
    expect(state.sessions).toEqual([]);
  });

  it("init() loads the initial session list", async () => {
    listSessions.mockResolvedValue([session()]);

    await useMixerStore.getState().init();

    const state = useMixerStore.getState();
    expect(state.isLoaded).toBe(true);
    expect(state.sessions).toHaveLength(1);
    expect(state.sessions[0].id).toBe("session-1");
  });

  it("init() is idempotent - calling it twice only subscribes once", async () => {
    await useMixerStore.getState().init();
    await useMixerStore.getState().init();

    expect(listSessions).toHaveBeenCalledTimes(1);
  });

  it("init() fetches and stores the backend's max volume percent", async () => {
    maxVolumePercent.mockResolvedValue(200);

    await useMixerStore.getState().init();
    await vi.waitFor(() => {
      expect(useMixerStore.getState().maxVolumePercent).toBe(200);
    });
  });

  it("init() leaves max volume percent at the default 100 if the fetch fails", async () => {
    maxVolumePercent.mockRejectedValue(new Error("no backend"));

    await useMixerStore.getState().init();

    expect(useMixerStore.getState().maxVolumePercent).toBe(100);
  });

  it("a sessions-changed event replaces the session list", async () => {
    await useMixerStore.getState().init();
    expect(capturedCallback.current).not.toBeNull();

    const pushed = [session({ id: "session-2", displayName: "Pushed App" })];
    capturedCallback.current!(pushed);

    expect(useMixerStore.getState().sessions).toEqual(pushed);
  });

  it("a push that omits iconPng keeps the icon already held for that session", async () => {
    await useMixerStore.getState().init();
    const icon = [1, 2, 3];
    useMixerStore.setState({ sessions: [session({ iconPng: icon })] });

    // The backend leaves `iconPng` out entirely once it has already delivered it -- see
    // `PushedSession` in lib.rs. Absent must mean "keep yours", not "this app has no icon".
    capturedCallback.current!([
      { ...session({ volume: 0.9 }), iconPng: undefined },
    ]);

    const [merged] = useMixerStore.getState().sessions;
    expect(merged.volume).toBe(0.9);
    expect(merged.iconPng).toBe(icon);
  });

  it("a push carrying an explicit null iconPng clears the icon", async () => {
    await useMixerStore.getState().init();
    useMixerStore.setState({ sessions: [session({ iconPng: [1, 2, 3] })] });

    capturedCallback.current!([session({ iconPng: null })]);

    expect(useMixerStore.getState().sessions[0].iconPng).toBeNull();
  });

  it("an icon that arrives mid-drag survives the later pushes that omit it", async () => {
    await useMixerStore.getState().init();
    useMixerStore.setState({
      sessions: [session({ id: "session-1", iconPng: null })],
      draggingSessionId: "session-1",
    });

    const icon = [7, 8, 9];
    // Only the most recent buffered push is applied when the drag ends, so an icon delivered by
    // an earlier one has to be carried into the buffer as it arrives.
    capturedCallback.current!([session({ id: "session-1", iconPng: icon })]);
    capturedCallback.current!([
      { ...session({ id: "session-1", volume: 0.9 }), iconPng: undefined },
    ]);

    useMixerStore.getState().setDraggingSessionId(null);

    const [merged] = useMixerStore.getState().sessions;
    expect(merged.volume).toBe(0.9);
    expect(merged.iconPng).toBe(icon);
  });

  it("a sessions-changed push keeps the actively-dragged session's existing object reference", async () => {
    await useMixerStore.getState().init();
    const original = session({ id: "session-1", volume: 0.5, effectiveVolume: 0.5 });
    useMixerStore.setState({ sessions: [original], draggingSessionId: "session-1" });

    // A fresh push reporting a *different* volume for the dragged session -- as a routine backend
    // poll would, since the drag is genuinely changing the real volume via direct backend calls.
    const pushed = [session({ id: "session-1", volume: 0.61, effectiveVolume: 0.61 })];
    capturedCallback.current!(pushed);

    const { sessions } = useMixerStore.getState();
    expect(sessions[0]).toBe(original);
  });

  it("a sessions-changed push is buffered (not applied to any session) while one is being dragged", async () => {
    await useMixerStore.getState().init();
    const dragged = session({ id: "session-1", volume: 0.5 });
    const other = session({ id: "session-2", volume: 0.3 });
    useMixerStore.setState({
      sessions: [dragged, other],
      draggingSessionId: "session-1",
    });

    // Not just the dragged session -- *any* store update during a drag re-registers every row
    // with Framer Motion's layout system for that frame, so even an unrelated session's push
    // must wait until the drag ends rather than applying immediately.
    const pushed = [
      session({ id: "session-1", volume: 0.9 }),
      session({ id: "session-2", volume: 0.7 }),
    ];
    capturedCallback.current!(pushed);

    const { sessions } = useMixerStore.getState();
    expect(sessions.find((s) => s.id === "session-1")).toBe(dragged);
    expect(sessions.find((s) => s.id === "session-2")).toBe(other);
  });

  it("applies the most recent buffered push once the drag ends", async () => {
    await useMixerStore.getState().init();
    const dragged = session({ id: "session-1", volume: 0.5 });
    const other = session({ id: "session-2", volume: 0.3 });
    useMixerStore.setState({
      sessions: [dragged, other],
      draggingSessionId: "session-1",
    });

    const firstPush = [
      session({ id: "session-1", volume: 0.6 }),
      session({ id: "session-2", volume: 0.4 }),
    ];
    const secondPush = [
      session({ id: "session-1", volume: 0.9 }),
      session({ id: "session-2", volume: 0.7 }),
    ];
    capturedCallback.current!(firstPush);
    capturedCallback.current!(secondPush);

    useMixerStore.getState().setDraggingSessionId(null);

    const { sessions, draggingSessionId } = useMixerStore.getState();
    expect(draggingSessionId).toBeNull();
    // Only the *most recent* buffered push applies -- an intermediate one arriving mid-drag is
    // superseded, not queued.
    expect(sessions.find((s) => s.id === "session-1")!.volume).toBe(0.9);
    expect(sessions.find((s) => s.id === "session-2")!.volume).toBe(0.7);
  });

  it("ending a drag with no buffered push just clears draggingSessionId", async () => {
    await useMixerStore.getState().init();
    const original = session({ id: "session-1", volume: 0.5 });
    useMixerStore.setState({ sessions: [original], draggingSessionId: "session-1" });

    useMixerStore.getState().setDraggingSessionId(null);

    const { sessions, draggingSessionId } = useMixerStore.getState();
    expect(draggingSessionId).toBeNull();
    expect(sessions[0]).toBe(original);
  });

  it("a sessions-changed push replaces everything normally once dragging ends", async () => {
    await useMixerStore.getState().init();
    useMixerStore.setState({
      sessions: [session({ id: "session-1", volume: 0.5 })],
      draggingSessionId: null,
    });

    const pushed = [session({ id: "session-1", volume: 0.9 })];
    capturedCallback.current!(pushed);

    expect(useMixerStore.getState().sessions).toEqual(pushed);
  });

  it("setDraggingSessionId() updates draggingSessionId", () => {
    useMixerStore.getState().setDraggingSessionId("session-1");
    expect(useMixerStore.getState().draggingSessionId).toBe("session-1");

    useMixerStore.getState().setDraggingSessionId(null);
    expect(useMixerStore.getState().draggingSessionId).toBeNull();
  });

  it("setVolume() optimistically updates local state immediately", () => {
    useMixerStore.setState({ sessions: [session({ volume: 0.2 })] });

    useMixerStore.getState().setVolume("session-1", 0.9);

    expect(useMixerStore.getState().sessions[0].volume).toBe(0.9);
    expect(setVolume).toHaveBeenCalledWith("session-1", 0.9);
  });

  it("setVolume() preserves an already-ducked session's duck ratio instead of showing full volume", () => {
    // Regression test: an outright `effectiveVolume: volume` assumed the session wasn't
    // currently ducked, so a volume change on an actively-ducked session briefly showed the
    // *full* volume before the next backend push corrected it back down -- a real, visible
    // ease-down flicker, not the harmless blip it was assumed to be.
    useMixerStore.setState({
      sessions: [
        session({ volume: 0.8, effectiveVolume: 0.24, isDucked: true }), // 30% duck ratio
      ],
    });

    useMixerStore.getState().setVolume("session-1", 0.4);

    const { sessions } = useMixerStore.getState();
    expect(sessions[0].volume).toBe(0.4);
    expect(sessions[0].effectiveVolume).toBeCloseTo(0.12, 5); // same 30% ratio, not 0.4
  });

  it("setVolume() shows full volume for a non-ducked session, as before", () => {
    useMixerStore.setState({
      sessions: [session({ volume: 0.2, effectiveVolume: 0.2, isDucked: false })],
    });

    useMixerStore.getState().setVolume("session-1", 0.9);

    expect(useMixerStore.getState().sessions[0].effectiveVolume).toBe(0.9);
  });

  it("setVolume() only updates the matching session", () => {
    useMixerStore.setState({
      sessions: [session({ id: "a", volume: 0.1 }), session({ id: "b", volume: 0.1 })],
    });

    useMixerStore.getState().setVolume("a", 0.7);

    const { sessions } = useMixerStore.getState();
    expect(sessions.find((s) => s.id === "a")!.volume).toBe(0.7);
    expect(sessions.find((s) => s.id === "b")!.volume).toBe(0.1);
  });

  it("setVolume() freezes the session against a stale backend echo for a moment", async () => {
    // Regression test: a plain click (no real drag, so `useDraggingSessionFreeze` never runs)
    // used to have zero protection against the backend's independently-scheduled poll loop
    // echoing back a pre-write snapshot and briefly overwriting the just-set optimistic value --
    // visibly, the slider snapping back toward the old value before correcting again.
    vi.useFakeTimers();
    await useMixerStore.getState().init();
    useMixerStore.setState({ sessions: [session({ volume: 0.2 })] });

    useMixerStore.getState().setVolume("session-1", 0.9);
    expect(useMixerStore.getState().draggingSessionId).toBe("session-1");

    // A stale push racing the write -- buffered, not applied, while frozen.
    capturedCallback.current!([session({ volume: 0.2 })]);
    expect(useMixerStore.getState().sessions[0].volume).toBe(0.9);

    // A later, correct push arrives and gets buffered too.
    capturedCallback.current!([session({ volume: 0.9 })]);

    vi.advanceTimersByTime(400);
    expect(useMixerStore.getState().draggingSessionId).toBeNull();
    expect(useMixerStore.getState().sessions[0].volume).toBe(0.9);
    vi.useRealTimers();
  });

  it("a stale gesture's release doesn't cut short a newer gesture's freeze on the same session", async () => {
    // Regression test: `setVolume` (a plain click) and the drag-freeze hook both arm the same
    // `draggingSessionId` field for the same session across a rapid sequence of separate
    // gestures. A session-id-only guard can't tell "my own release" from "some other, earlier
    // gesture's release firing late" -- confirmed live as the actual root cause of the reported
    // flicker (a freeze cut short after ~24ms instead of 400ms). `draggingGeneration` is the
    // fix: only the release that captured the *current* generation may actually clear it.
    await useMixerStore.getState().init();
    useMixerStore.setState({ sessions: [session({ volume: 0.2 })] });

    useMixerStore.getState().setVolume("session-1", 0.5);
    const staleGeneration = useMixerStore.getState().draggingGeneration;

    // A second, later gesture on the *same* session re-arms the freeze before the first
    // gesture's own release ever fires.
    useMixerStore.getState().setVolume("session-1", 0.9);
    expect(useMixerStore.getState().draggingGeneration).not.toBe(staleGeneration);

    // The first gesture's now-stale release call must not touch the second gesture's freeze.
    useMixerStore.getState().endFreezeIfCurrent("session-1", staleGeneration);
    expect(useMixerStore.getState().draggingSessionId).toBe("session-1");

    // The second gesture's own (current) release call does clear it.
    useMixerStore
      .getState()
      .endFreezeIfCurrent("session-1", useMixerStore.getState().draggingGeneration);
    expect(useMixerStore.getState().draggingSessionId).toBeNull();
  });

  it("setVolume() doesn't steal the freeze from a different session that's actively being dragged", async () => {
    vi.useFakeTimers();
    await useMixerStore.getState().init();
    useMixerStore.setState({
      sessions: [session({ id: "session-1", volume: 0.5 }), session({ id: "session-2", volume: 0.2 })],
      draggingSessionId: "session-1",
    });

    useMixerStore.getState().setVolume("session-2", 0.9);

    expect(useMixerStore.getState().draggingSessionId).toBe("session-1");
    vi.advanceTimersByTime(400);
    // Still frozen for session-1 -- setVolume's own timeout for session-2 must not have fired
    // and cleared it, since it never actually acquired the freeze in the first place.
    expect(useMixerStore.getState().draggingSessionId).toBe("session-1");
    vi.useRealTimers();
  });

  it("setMuted() optimistically updates local state immediately", () => {
    useMixerStore.setState({ sessions: [session({ muted: false })] });

    useMixerStore.getState().setMuted("session-1", true);

    expect(useMixerStore.getState().sessions[0].muted).toBe(true);
    expect(setMuted).toHaveBeenCalledWith("session-1", true);
  });

  it("setBalance() optimistically updates local state immediately", () => {
    useMixerStore.setState({ sessions: [session({ balance: 0 })] });

    useMixerStore.getState().setBalance("session-1", 0.7);

    expect(useMixerStore.getState().sessions[0].balance).toBe(0.7);
    expect(setBalance).toHaveBeenCalledWith("session-1", 0.7);
  });

  it("init() sets needsPermission when the backend reports the permission-wait error", async () => {
    vi.useFakeTimers();
    listSessions.mockRejectedValue(
      "platform audio API error: waiting for screen & system audio recording permission",
    );

    await useMixerStore.getState().init();

    expect(useMixerStore.getState().needsPermission).toBe(true);
    expect(useMixerStore.getState().isLoaded).toBe(false);

    // Whether a mid-session permission grant takes effect without a full
    // app relaunch is inconsistent in practice, so the store must keep
    // checking rather than latching "needs permission" forever -- confirm
    // it keeps retrying instead of giving up after the first failure.
    await vi.advanceTimersByTimeAsync(6000);
    expect(listSessions.mock.calls.length).toBeGreaterThan(1);

    vi.useRealTimers();
  });

  it("init() clears needsPermission once a retry succeeds after the permission is actually granted", async () => {
    vi.useFakeTimers();
    listSessions.mockRejectedValueOnce(
      "platform audio API error: waiting for screen & system audio recording permission",
    );
    listSessions.mockResolvedValueOnce([session()]);

    await useMixerStore.getState().init();
    expect(useMixerStore.getState().needsPermission).toBe(true);

    await vi.advanceTimersByTimeAsync(2000);

    expect(useMixerStore.getState().needsPermission).toBe(false);
    expect(useMixerStore.getState().isLoaded).toBe(true);

    vi.useRealTimers();
  });

  it("init() retries on a generic (non-permission) failure and recovers once it succeeds", async () => {
    vi.useFakeTimers();
    listSessions.mockRejectedValueOnce(new Error("transient IPC failure"));
    listSessions.mockResolvedValueOnce([session()]);

    await useMixerStore.getState().init();
    expect(useMixerStore.getState().isLoaded).toBe(false);
    expect(useMixerStore.getState().needsPermission).toBe(false);

    await vi.advanceTimersByTimeAsync(2000);

    expect(useMixerStore.getState().isLoaded).toBe(true);
    expect(useMixerStore.getState().sessions).toHaveLength(1);

    vi.useRealTimers();
  });

  it("setVolume() still updates local state even if the backend call rejects", async () => {
    setVolume.mockRejectedValue(new Error("backend down"));
    useMixerStore.setState({ sessions: [session({ volume: 0.2 })] });

    useMixerStore.getState().setVolume("session-1", 0.6);

    expect(useMixerStore.getState().sessions[0].volume).toBe(0.6);
    // Let the rejected promise's .catch() handler run without throwing.
    await Promise.resolve();
    await Promise.resolve();
  });
});
