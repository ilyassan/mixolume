import { describe, it, expect } from "vitest";
import { toLeftRight, fromLeftRight } from "./SessionRow";

describe("toLeftRight", () => {
  it("centered balance gives both channels the full volume", () => {
    expect(toLeftRight(0.8, 0)).toEqual([0.8, 0.8]);
  });

  it("full right balance silences the left channel only", () => {
    const [left, right] = toLeftRight(0.8, 1);
    expect(left).toBeCloseTo(0);
    expect(right).toBeCloseTo(0.8);
  });

  it("full left balance silences the right channel only", () => {
    const [left, right] = toLeftRight(0.8, -1);
    expect(left).toBeCloseTo(0.8);
    expect(right).toBeCloseTo(0);
  });
});

describe("fromLeftRight", () => {
  it("equal channels give centered balance at that volume", () => {
    const [volume, balance] = fromLeftRight(0.6, 0.6);
    expect(volume).toBeCloseTo(0.6);
    expect(balance).toBeCloseTo(0);
  });

  it("louder right channel becomes the volume, with positive balance", () => {
    const [volume, balance] = fromLeftRight(0.3, 0.8);
    expect(volume).toBeCloseTo(0.8);
    expect(balance).toBeCloseTo(0.625);
  });

  it("louder left channel becomes the volume, with negative balance", () => {
    const [volume, balance] = fromLeftRight(0.9, 0.4);
    expect(volume).toBeCloseTo(0.9);
    expect(balance).toBeCloseTo(-0.5556, 3);
  });

  it("both channels silent gives zero volume and centered balance", () => {
    expect(fromLeftRight(0, 0)).toEqual([0, 0]);
  });

  it("round-trips through toLeftRight for a range of independent left/right pairs", () => {
    const cases: [number, number][] = [
      [0.5, 0.5],
      [0.3, 0.8],
      [0.9, 0.4],
      [1, 0],
      [0, 1],
      [0.75, 0.25],
      [0.1, 0.9],
    ];
    for (const [left, right] of cases) {
      const [volume, balance] = fromLeftRight(left, right);
      const [roundTrippedLeft, roundTrippedRight] = toLeftRight(volume, balance);
      expect(roundTrippedLeft).toBeCloseTo(left, 5);
      expect(roundTrippedRight).toBeCloseTo(right, 5);
    }
  });
});
