import { describe, expect, it } from "vitest";
import { MsgRedeemTokensForSharesType, MsgTokenizeSharesType, parseDec18, shareTokenValidator, sharesToTokens } from "./lsm";

describe("shareTokenValidator", () => {
  it("accepts <valoper>/<id> only", () => {
    expect(shareTokenValidator("cosmosvaloper1abc/12")).toBe("cosmosvaloper1abc");
    for (const bad of ["uatom", "cosmos1abc/12", "cosmosvaloper1abc/", "cosmosvaloper1abc/x", "factory/cosmos1x/liatom", "a/b/1"]) {
      expect(shareTokenValidator(bad)).toBeNull();
    }
  });
});

describe("MsgTokenizeShares codec", () => {
  const msg = {
    delegatorAddress: "cosmos1x",
    validatorAddress: "cosmosvaloper1y",
    amount: { denom: "uatom", amount: "5" },
    tokenizedShareOwner: "cosmos1x",
  };
  it("writes the gaia field numbers", () => {
    const bytes = MsgTokenizeSharesType.encode(msg).finish();
    const want = [
      0x0a, 8, ...new TextEncoder().encode("cosmos1x"),
      0x12, 15, ...new TextEncoder().encode("cosmosvaloper1y"),
      0x1a, 10, 0x0a, 5, ...new TextEncoder().encode("uatom"), 0x12, 1, ...new TextEncoder().encode("5"),
      0x22, 8, ...new TextEncoder().encode("cosmos1x"),
    ];
    expect(Array.from(bytes)).toEqual(want);
  });
  it("round-trips", () => {
    const bytes = MsgTokenizeSharesType.encode(msg).finish();
    expect(MsgTokenizeSharesType.decode(bytes)).toEqual(msg);
  });
});

describe("MsgRedeemTokensForShares codec", () => {
  it("writes the gaia field numbers and round-trips", () => {
    const msg = { delegatorAddress: "cosmos1x", amount: { denom: "v/1", amount: "5" } };
    const bytes = MsgRedeemTokensForSharesType.encode(msg).finish();
    const want = [
      0x0a, 8, ...new TextEncoder().encode("cosmos1x"),
      0x12, 8, 0x0a, 3, ...new TextEncoder().encode("v/1"), 0x12, 1, ...new TextEncoder().encode("5"),
    ];
    expect(Array.from(bytes)).toEqual(want);
    expect(MsgRedeemTokensForSharesType.decode(bytes)).toEqual(msg);
  });
});

describe("share token value", () => {
  it("parses both Dec formats", () => {
    expect(parseDec18("12.5")).toBe(12_500_000_000_000_000_000n);
    expect(parseDec18("12500000000000000000")).toBe(12_500_000_000_000_000_000n);
  });
  it("converts shares to tokens (a slashed validator has fewer tokens than shares)", () => {
    const shares = parseDec18("1000000.000000000000000000");
    expect(sharesToTokens(100n, 1_000_000n, shares)).toBe(100n);
    expect(sharesToTokens(1_000_000n, 990_000n, shares)).toBe(990_000n);
    expect(sharesToTokens(7n, 5n, 0n)).toBe(7n);
  });
});
