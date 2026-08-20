import { describe, it, expect, vi, beforeEach } from "vitest";

const { listSessions, setVolume, setMuted, capturedCallback, unlistenMock } =
  vi.hoisted(() => {
    const unlistenMock = vi.fn();
    return {
      listSessions: vi.fn(),
      setVolume: vi.fn(),
      setMuted: vi.fn(),
      capturedCallback: { current: null as null | ((s: unknown[]) => void) },
      unlistenMock,
    };
  });

vi.mock("@/lib/tauri", () => ({
  listSessions: (...args: unknown[]) => listSessions(...args),
  setVolume: (...args: unknown[]) => setVolume(...args),
  setMuted: (...args: unknown[]) => setMuted(...args),
  listenToSessionsChanged: vi.fn((callback: (s: unknown[]) => void) => {
    capturedCallback.current = callback;
    return Promise.resolve(unlistenMock);
  }),
}));

import { useMixerStore } from "./mixer-store";
import type { AppSession } from "@/lib/tauri";

const session = (overrides: Partial<AppSession> = {}): AppSession => ({
  id: "session-1",
  displayName: "Test App",
  iconPng: null,
  volume: 0.5,
  muted: false,
  isActive: true,
  ...overrides,
});

describe("mixer-store", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    capturedCallback.current = null;
    listSessions.mockResolvedValue([]);
    setVolume.mockResolvedValue(undefined);
    setMuted.mockResolvedValue(undefined);
    useMixerStore.setState({
      sessions: [],
      isLoaded: false,
      isInitialized: false,
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
