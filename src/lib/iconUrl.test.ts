import { describe, it, expect } from "vitest";
import { iconPngToDataUrl } from "./iconUrl";

describe("iconPngToDataUrl", () => {
  it("returns null for null input", () => {
    expect(iconPngToDataUrl(null)).toBeNull();
  });

  it("returns null for undefined input", () => {
    expect(iconPngToDataUrl(undefined)).toBeNull();
  });

  it("returns null for an empty array", () => {
    expect(iconPngToDataUrl([])).toBeNull();
  });

  it("encodes bytes into a base64 PNG data URL", () => {
    // A minimal byte sequence - the PNG magic number header.
    const pngHeader = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    const url = iconPngToDataUrl(pngHeader);

    expect(url).toMatch(/^data:image\/png;base64,/);

    const base64 = url!.slice("data:image/png;base64,".length);
    const decoded = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    expect(Array.from(decoded)).toEqual(pngHeader);
  });

  it("round-trips a larger byte array spanning multiple encoding chunks", () => {
    const bytes = Array.from({ length: 100_000 }, (_, i) => i % 256);
    const url = iconPngToDataUrl(bytes);
    const base64 = url!.slice("data:image/png;base64,".length);
    const decoded = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    expect(Array.from(decoded)).toEqual(bytes);
  });
});
