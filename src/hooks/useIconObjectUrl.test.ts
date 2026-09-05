import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useIconObjectUrl } from "./useIconObjectUrl";

// jsdom's own `Blob`/`URL.createObjectURL` work fine in this environment as long as the hook
// itself doesn't loop (see `useIconObjectUrl.ts`'s `iconKey` comment for the real bug this test
// file's own earlier flakiness traced back to -- not a jsdom issue). Only `URL`'s two methods are
// mocked here, not `Blob` itself, and via direct property assignment rather than a full `URL`
// rebind -- replacing the whole `URL` global breaks `new URL(...)` elsewhere in the environment.
let nextUrl = 0;
let createObjectURLSpy: ReturnType<typeof vi.fn>;
let revokeObjectURLSpy: ReturnType<typeof vi.fn>;
let originalCreateObjectURL: typeof URL.createObjectURL;
let originalRevokeObjectURL: typeof URL.revokeObjectURL;

beforeEach(() => {
  nextUrl = 0;
  createObjectURLSpy = vi.fn(() => `blob:mock-${nextUrl++}`);
  revokeObjectURLSpy = vi.fn();
  originalCreateObjectURL = URL.createObjectURL;
  originalRevokeObjectURL = URL.revokeObjectURL;
  URL.createObjectURL = createObjectURLSpy as unknown as typeof URL.createObjectURL;
  URL.revokeObjectURL = revokeObjectURLSpy as unknown as typeof URL.revokeObjectURL;
});

afterEach(() => {
  URL.createObjectURL = originalCreateObjectURL;
  URL.revokeObjectURL = originalRevokeObjectURL;
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

  it("does not recreate the object URL when given a new array with identical bytes", () => {
    const { result, rerender } = renderHook(
      ({ iconPng }) => useIconObjectUrl(iconPng),
      { initialProps: { iconPng: [1, 2, 3] as number[] | null } },
    );
    expect(result.current).toBe("blob:mock-0");

    act(() => {
      // A brand new array reference, same bytes -- must not re-trigger the effect.
      rerender({ iconPng: [1, 2, 3] });
    });

    expect(createObjectURLSpy).toHaveBeenCalledTimes(1);
    expect(revokeObjectURLSpy).not.toHaveBeenCalled();
    expect(result.current).toBe("blob:mock-0");
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
