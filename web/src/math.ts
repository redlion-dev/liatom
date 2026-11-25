// Pure integer math mirroring the contract. All amounts are bigint in micro units (6 decimals).

export const DECIMALS = 6;

/** Parses "12.5" into micro units. Returns null for anything that is not a plain decimal number. */
export function parseAmount(text: string, decimals = DECIMALS): bigint | null {
  const t = text.trim().replace(",", ".");
  if (!/^\d*\.?\d*$/.test(t) || t === "" || t === ".") return null;
  const [whole, frac = ""] = t.split(".");
  if (frac.length > decimals) return null;
  return BigInt(whole || "0") * 10n ** BigInt(decimals) + BigInt(frac.padEnd(decimals, "0") || "0");
}

/** 1234567n -> "1.234567", trailing zeros trimmed, at most `maxFrac` fraction digits (truncated). */
export function formatAmount(v: bigint, maxFrac = DECIMALS, decimals = DECIMALS): string {
  const base = 10n ** BigInt(decimals);
  const whole = v / base;
  let frac = (v % base).toString().padStart(decimals, "0").slice(0, maxFrac).replace(/0+$/, "");
  const w = whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, " ");
  return frac ? `${w}.${frac}` : w;
}

/** liATOM minted for a deposit, as the contract computes it (rounds down). */
export function estimateMint(atom: bigint, supplyEff: bigint, poolValue: bigint): bigint {
  if (poolValue === 0n) return 0n;
  return (atom * supplyEff) / poolValue;
}

/** ATOM a redemption is worth right now (the contract fixes the final amount at submit time). */
export function estimateRedeem(liatom: bigint, supplyEff: bigint, poolValue: bigint): bigint {
  if (supplyEff === 0n) return 0n;
  return (liatom * poolValue) / supplyEff;
}

/** Exact payout of a claim in a matured batch (floor of the pro-rata share). */
export function claimPayout(received: bigint, liatom: bigint, burned: bigint): bigint {
  if (burned === 0n) return 0n;
  return (received * liatom) / burned;
}

/** The contract serialises Timestamp as a string of nanoseconds. */
export function nanosToSecs(nanos: string): number {
  return Number(BigInt(nanos) / 1_000_000_000n);
}

export function formatDuration(secs: number): { days: number; hours: number; minutes: number } {
  const s = Math.max(0, Math.floor(secs));
  return { days: Math.floor(s / 86400), hours: Math.floor((s % 86400) / 3600), minutes: Math.floor((s % 3600) / 60) };
}
