type Dict = Record<string, string>;

const en: Dict = {
  connect: "Connect wallet", disconnect: "Disconnect", connecting: "Connecting...",
  rate: "1 liATOM =", tvl: "Total staked", supply: "liATOM supply", fee: "Protocol fee",
  stake: "Stake", unstake: "Unstake", claims: "Claims", lsm: "Staked ATOM",
  lsmIntro: "Already staking ATOM? Move it into liATOM without the 21-day wait: convert your delegation into a share token, then stake the token. Your stake keeps earning and is not unbonded.",
  lsmStep1: "1. Convert a delegation into a share token", lsmStep2: "2. Stake a share token for liATOM",
  lsmNoDelegations: "No delegations found for this wallet.", lsmNoTokens: "No share tokens in this wallet yet.",
  lsmConnect: "Connect a wallet to see your delegations.",
  lsmNotAccepted: "Not accepted: this validator is not in the protocol's set.",
  unwrap: "To delegation", unstakeShares: "Unstake now as staked ATOM",
  unstakeSharesNote: "Or leave at once: your liATOM is burned at the current rate and you receive the same value as a share token (an LSM token of a staked position that keeps earning). Redeem the token into a delegation under the Staked ATOM tab; unbonding it later takes the usual 21 days. May fail if the network's liquid-staking limit is reached; the regular unstake above always works.",
  convert: "Convert", stakeToken: "Stake", lsmGet: "you receive ≈",
  lsmNote: "Converting needs a wallet that signs with the standard (direct) mode; some hardware-wallet modes cannot sign it. Your delegation's rewards are paid when you convert.",
  youStake: "You stake", youUnstake: "You unstake", youGet: "You receive ≈", worthNow: "Worth now ≈",
  balance: "Balance", max: "Max",
  noClaims: "No pending withdrawals.",
  connectFirst: "Connect a wallet first.",
  belowMin: "Minimum deposit is {min} ATOM.",
  notEnough: "Not enough balance.",
  capReached: "Deposit limit reached. Room left: {room} ATOM.",
  pausedDeposits: "Deposits are paused. You can still unstake and claim.",
  pausedFull: "The protocol is paused: deposits and new unstake requests are disabled. Claims still work.",
  unstakeNote: "Unstaking takes about {days}: your request joins a batch sent for unbonding within {epoch}, then the network unbonding period of {unb} applies. The ATOM amount is fixed when the batch is sent, and no rewards accrue meanwhile.",
  statusOpen: "Waiting to be sent for unbonding", statusUnbonding: "Unbonding, ready in {t}",
  statusFinalize: "Unbonding finished, needs to be finalized", statusReady: "Ready to claim",
  claim: "Claim", finalize: "Finalize", batch: "Batch",
  sent: "Transaction confirmed", failed: "Transaction failed", walletMissing: "No Keplr or Leap wallet found.",
  amountInvalid: "Enter a valid amount.",
  notLaunched: "liATOM is not launched yet. This page is a preview: no contract is configured.",
  d: "d", h: "h", m: "min",
};

export function t(key: string, vars: Record<string, string | number> = {}): string {
  let s = en[key] ?? key;
  for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v));
  return s;
}

export function applyStaticText(): void {
  document.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    el.textContent = t(el.dataset.i18n!);
  });
}
