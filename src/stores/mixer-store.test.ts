import { describe, it, expect, vi, beforeEach } from "vitest";

const { listSessions, setVolume, setMuted, setBalance, capturedCallback, unlistenMock } =
  vi.hoisted(() => {
    const unlistenMock = vi.fn();
    return {
      listSessions: vi.fn(),
      setVolume: vi.fn(),
      setMuted: vi.fn(),
      setBalance: vi.fn(),
      capturedCallback: { current: null as null | ((s: unknown[]) => void) },
      unlistenMock,
    };
  });

vi.mock("@/lib/tauri", () => ({
  listSessions: (...args: unknown[]) => listSessions(...args),
  setVolume: (...args: unknown[]) => setVolume(...args),
  setMuted: (...args: unknown[]) => setMuted(...args),
  setBalance: (...args: unknown[]) => setBalance(...args),
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

import { useMixerStore } from "./mixer-store";
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
  ...overrides,
});

describe("mixer-store", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    capturedCallback.current = null;
    listSessions.mockResolvedValue([]);
    setVolume.mockResolvedValue(undefined);
    setMuted.mockResolvedValue(undefined);
    setBalance.mockResolvedValue(undefined);
    useMixerStore.setState({
      sessions: [],
      isLoaded: false,
      isInitialized: false,
      needsPermission: false,
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

  it("a sessions-changed event replaces the session list", async () => {
    await useMixerStore.getState().init();
    expect(capturedCallback.current).not.toBeNull();

    const pushed = [session({ id: "session-2", displayName: "Pushed App" })];
    capturedCallback.current!(pushed);

    expect(useMixerStore.getState().sessions).toEqual(pushed);
  });

  it("setVolume() optimistically updates local state immediately", () => {
    useMixerStore.setState({ sessions: [session({ volume: 0.2 })] });

    useMixerStore.getState().setVolume("session-1", 0.9);

    expect(useMixerStore.getState().sessions[0].volume).toBe(0.9);
    expect(setVolume).toHaveBeenCalledWith("session-1", 0.9);
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
