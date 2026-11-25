// Liquid staking module (LSM, x/liquid on the Cosmos Hub): tokenizing a delegation.
//
// A delegation becomes a share token `<validator operator address>/<record id>`; sending that token to the
// hub (`deposit_tokenized_shares`) turns it into liATOM without the 21-day unbonding wait.

import type { GeneratedType } from "@cosmjs/proto-signing";
import { BinaryReader, BinaryWriter } from "cosmjs-types/binary";
import { Coin } from "cosmjs-types/cosmos/base/v1beta1/coin";

export const TOKENIZE_TYPE_URL = "/gaia.liquid.v1beta1.MsgTokenizeShares";

export interface MsgTokenizeShares {
  delegatorAddress: string;
  validatorAddress: string;
  amount?: { denom: string; amount: string };
  tokenizedShareOwner: string;
}

/** Minimal protobuf codec for the one LSM message the app signs (cosmjs ships no LSM types). */
export const MsgTokenizeSharesType: GeneratedType = {
  encode(m: MsgTokenizeShares, w: BinaryWriter = BinaryWriter.create()): BinaryWriter {
    if (m.delegatorAddress) w.uint32(10).string(m.delegatorAddress);
    if (m.validatorAddress) w.uint32(18).string(m.validatorAddress);
    if (m.amount) Coin.encode(m.amount, w.uint32(26).fork()).ldelim();
    if (m.tokenizedShareOwner) w.uint32(34).string(m.tokenizedShareOwner);
    return w;
  },
  decode(input: BinaryReader | Uint8Array, length?: number): MsgTokenizeShares {
    const r = input instanceof BinaryReader ? input : new BinaryReader(input);
    const end = length === undefined ? r.len : r.pos + length;
    const m: MsgTokenizeShares = { delegatorAddress: "", validatorAddress: "", tokenizedShareOwner: "" };
    while (r.pos < end) {
      const tag = r.uint32();
      switch (tag >>> 3) {
        case 1: m.delegatorAddress = r.string(); break;
        case 2: m.validatorAddress = r.string(); break;
        case 3: m.amount = Coin.decode(r, r.uint32()); break;
        case 4: m.tokenizedShareOwner = r.string(); break;
        default: r.skipType(tag & 7);
      }
    }
    return m;
  },
  fromPartial(o: Partial<MsgTokenizeShares>): MsgTokenizeShares {
    return {
      delegatorAddress: o.delegatorAddress ?? "",
      validatorAddress: o.validatorAddress ?? "",
      amount: o.amount,
      tokenizedShareOwner: o.tokenizedShareOwner ?? "",
    };
  },
};

export const REDEEM_TYPE_URL = "/gaia.liquid.v1beta1.MsgRedeemTokensForShares";

export interface MsgRedeemTokensForShares {
  delegatorAddress: string;
  amount?: { denom: string; amount: string };
}

/** Codec for the message that turns a share token back into a delegation of the signer. */
export const MsgRedeemTokensForSharesType: GeneratedType = {
  encode(m: MsgRedeemTokensForShares, w: BinaryWriter = BinaryWriter.create()): BinaryWriter {
    if (m.delegatorAddress) w.uint32(10).string(m.delegatorAddress);
    if (m.amount) Coin.encode(m.amount, w.uint32(18).fork()).ldelim();
    return w;
  },
  decode(input: BinaryReader | Uint8Array, length?: number): MsgRedeemTokensForShares {
    const r = input instanceof BinaryReader ? input : new BinaryReader(input);
    const end = length === undefined ? r.len : r.pos + length;
    const m: MsgRedeemTokensForShares = { delegatorAddress: "" };
    while (r.pos < end) {
      const tag = r.uint32();
      switch (tag >>> 3) {
        case 1: m.delegatorAddress = r.string(); break;
        case 2: m.amount = Coin.decode(r, r.uint32()); break;
        default: r.skipType(tag & 7);
      }
    }
    return m;
  },
  fromPartial(o: Partial<MsgRedeemTokensForShares>): MsgRedeemTokensForShares {
    return { delegatorAddress: o.delegatorAddress ?? "", amount: o.amount };
  },
};

/** The validator of a share token denom `<valoper>/<record id>`, or null for any other denom. */
export function shareTokenValidator(denom: string): string | null {
  const m = /^([a-z0-9]+)\/(\d{1,20})$/.exec(denom);
  if (!m || !m[1].includes("valoper")) return null;
  return m[1];
}

/** A cosmos Dec as it comes from the chain, as an integer scaled by 1e18 ("12.5" and "12500000000000000000" both work). */
export function parseDec18(s: string): bigint {
  if (s.includes(".")) {
    const [w, f = ""] = s.split(".");
    return BigInt(w || "0") * 10n ** 18n + BigInt(f.padEnd(18, "0").slice(0, 18) || "0");
  }
  return BigInt(s);
}

/** What a share token amount is worth in ATOM: shares x (validator tokens / validator delegator shares), rounded down. */
export function sharesToTokens(shares: bigint, validatorTokens: bigint, validatorShares18: bigint): bigint {
  if (validatorShares18 <= 0n) return shares;
  return (shares * validatorTokens * 10n ** 18n) / validatorShares18;
}
