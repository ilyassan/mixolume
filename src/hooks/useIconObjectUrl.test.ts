import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useIconObjectUrl } from "./useIconObjectUrl";

// jsdom's own `Blob` and `URL.createObjectURL`/`revokeObjectURL` are unreliable in this test
// environment -- confirmed live that even with only the `URL` methods mocked, jsdom's real
// `Blob` constructor pins a vitest worker process at 100%+ CPU indefinitely (eventually OOMing
// the worker) on nothing more than a 3-4 byte array. Both are mocked explicitly here rather than
// relied on. What's under test is this hook's *lifecycle* logic (create once per distinct byte
// array, revoke the previous one, revoke on unmount), not the browser APIs themselves.
let nextUrl = 0;
let createObjectURLSpy: ReturnType<typeof vi.fn>;
let revokeObjectURLSpy: ReturnType<typeof vi.fn>;

beforeEach(() => {
  nextUrl = 0;
  createObjectURLSpy = vi.fn(() => `blob:mock-${nextUrl++}`);
  revokeObjectURLSpy = vi.fn();
  vi.stubGlobal("URL", {
    ...URL,
    createObjectURL: createObjectURLSpy,
    revokeObjectURL: revokeObjectURLSpy,
  });
  vi.stubGlobal("Blob", vi.fn());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("useIconObjectUrl", () => {
  it("returns null for null input", () => {
    const { result } = renderHook(() => useIconObjectUrl(null));
    expect(result.current).toBeNull();
    expect(createObjectURLSpy).not.toHaveBeenCalled();
  });

  it("returns null for an empty array", () => {
    const { result } = renderHook(() => useIconObjectUrl([]));
    expect(result.current).toBeNull();
    expect(createObjectURLSpy).not.toHaveBeenCalled();
  });

  it("creates an object URL for non-empty bytes", () => {
    const { result } = renderHook(() => useIconObjectUrl([0x89, 0x50, 0x4e, 0x47]));
    expect(result.current).toBe("blob:mock-0");
    expect(createObjectURLSpy).toHaveBeenCalledTimes(1);
  });

  it("revokes the previous object URL when the bytes change", () => {
    const { result, rerender } = renderHook(
      ({ iconPng }) => useIconObjectUrl(iconPng),
      { initialProps: { iconPng: [1, 2, 3] as number[] | null } },
    );
    const firstUrl = result.current;
    expect(firstUrl).toBe("blob:mock-0");

    act(() => {
      rerender({ iconPng: [4, 5, 6] });
    });

    expect(revokeObjectURLSpy).toHaveBeenCalledWith(firstUrl);
    expect(result.current).toBe("blob:mock-1");
  });

  it("revokes the object URL on unmount", () => {
    const { result, unmount } = renderHook(() => useIconObjectUrl([1, 2, 3]));
    const url = result.current;

    unmount();

    expect(revokeObjectURLSpy).toHaveBeenCalledWith(url);
  });

  it("revokes the old URL and returns null when bytes become null", () => {
    const { result, rerender } = renderHook(
      ({ iconPng }) => useIconObjectUrl(iconPng),
      { initialProps: { iconPng: [1, 2, 3] as number[] | null } },
    );
    const firstUrl = result.current;

    act(() => {
      rerender({ iconPng: null });
    });

    expect(revokeObjectURLSpy).toHaveBeenCalledWith(firstUrl);
    expect(result.current).toBeNull();
  });
});
