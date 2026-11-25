import { describe, expect, it } from "vitest";
import {
  claimPayout, estimateMint, estimateRedeem, formatAmount, formatDuration, nanosToSecs, parseAmount,
} from "./math";

describe("parseAmount", () => {
  it("parses plain decimals", () => {
    expect(parseAmount("1")).toBe(1_000_000n);
    expect(parseAmount("12.5")).toBe(12_500_000n);
    expect(parseAmount("0.000001")).toBe(1n);
    expect(parseAmount(".5")).toBe(500_000n);
    expect(parseAmount("1,5")).toBe(1_500_000n);
  });
  it("rejects garbage, signs, exponents and too many digits", () => {
    for (const bad of ["", ".", "abc", "-1", "1e3", "1.0000001", "1.2.3", "0x10", " "]) {
      expect(parseAmount(bad), bad).toBeNull();
    }
  });
});

describe("formatAmount", () => {
  it("formats with grouping and trims zeros", () => {
    expect(formatAmount(0n)).toBe("0");
    expect(formatAmount(1_500_000n)).toBe("1.5");
    expect(formatAmount(1_234_567_890_000n)).toBe("1 234 567.89");
    expect(formatAmount(1n)).toBe("0.000001");
    expect(formatAmount(1_999_999n, 2)).toBe("1.99"); // truncates, never rounds up
  });
});

describe("contract math", () => {
  it("mint and redeem round down", () => {
    expect(estimateMint(10n, 3n, 4n)).toBe(7n); // 7.5
    expect(estimateRedeem(10n, 4n, 3n)).toBe(7n);
    expect(estimateMint(5n, 10n, 0n)).toBe(0n);
    expect(estimateRedeem(5n, 0n, 10n)).toBe(0n);
  });
  it("claim payout is the floored pro-rata share", () => {
    expect(claimPayout(2_000_001n, 1_000_000n, 2_000_000n)).toBe(1_000_000n);
    expect(claimPayout(10n, 1n, 3n)).toBe(3n);
    expect(claimPayout(10n, 1n, 0n)).toBe(0n);
  });
  it("large values stay exact", () => {
    const big = 10n ** 18n;
    expect(estimateMint(big, big, big)).toBe(big);
  });
});

describe("time", () => {
  it("converts nanoseconds and durations", () => {
    expect(nanosToSecs("1795358883701035354")).toBe(1795358883);
    expect(formatDuration(90061)).toEqual({ days: 1, hours: 1, minutes: 1 });
    expect(formatDuration(-5)).toEqual({ days: 0, hours: 0, minutes: 0 });
  });
});
