import { describe, it, expect } from "vitest";
import { balanceToChannels, balanceFromLeftFraction, balanceFromRightFraction } from "./balance";

describe("balanceToChannels", () => {
  it("centered balance gives both channels full", () => {
    expect(balanceToChannels(0)).toEqual([1, 1]);
  });

  it("full right balance zeroes only the left channel", () => {
    const [left, right] = balanceToChannels(1);
    expect(left).toBeCloseTo(0);
    expect(right).toBeCloseTo(1);
  });

  it("full left balance zeroes only the right channel", () => {
    const [left, right] = balanceToChannels(-1);
    expect(left).toBeCloseTo(1);
    expect(right).toBeCloseTo(0);
  });

  it("is independent of volume entirely -- volume isn't even a parameter", () => {
    // Same balance always gives the same channel fractions, regardless of whatever volume the
    // caller happens to be at -- that's the whole point of the split.
    expect(balanceToChannels(0.5)).toEqual(balanceToChannels(0.5));
  });
});

describe("balanceFromLeftFraction", () => {
  it("left at full gives centered balance (right becomes/stays full)", () => {
    expect(balanceFromLeftFraction(1)).toBeCloseTo(0);
  });

  it("left reduced gives positive balance", () => {
    expect(balanceFromLeftFraction(0.4)).toBeCloseTo(0.6);
  });

  it("left silenced gives full positive balance", () => {
    expect(balanceFromLeftFraction(0)).toBeCloseTo(1);
  });

  it("clamps out-of-range input", () => {
    expect(balanceFromLeftFraction(1.5)).toBeCloseTo(0);
    expect(balanceFromLeftFraction(-0.5)).toBeCloseTo(1);
  });

  it("round-trips through balanceToChannels for the left channel", () => {
    for (const leftFraction of [0, 0.25, 0.5, 0.75, 1]) {
      const [roundTrippedLeft] = balanceToChannels(balanceFromLeftFraction(leftFraction));
      expect(roundTrippedLeft).toBeCloseTo(leftFraction);
    }
  });
});

describe("balanceFromRightFraction", () => {
  it("right at full gives centered balance (left becomes/stays full)", () => {
    expect(balanceFromRightFraction(1)).toBeCloseTo(0);
  });

  it("right reduced gives negative balance", () => {
    expect(balanceFromRightFraction(0.4)).toBeCloseTo(-0.6);
  });

  it("right silenced gives full negative balance", () => {
    expect(balanceFromRightFraction(0)).toBeCloseTo(-1);
  });

  it("clamps out-of-range input", () => {
    expect(balanceFromRightFraction(1.5)).toBeCloseTo(0);
    expect(balanceFromRightFraction(-0.5)).toBeCloseTo(-1);
  });

  it("round-trips through balanceToChannels for the right channel", () => {
    for (const rightFraction of [0, 0.25, 0.5, 0.75, 1]) {
      const [, roundTrippedRight] = balanceToChannels(balanceFromRightFraction(rightFraction));
      expect(roundTrippedRight).toBeCloseTo(rightFraction);
    }
  });
});
