import {
  Chain, loadConfig, type Batch, type ContractConfig, type Delegation, type Pool, type Session, type ShareToken,
} from "./chain";
import { applyStaticText, t } from "./i18n";
import {
  claimPayout, estimateMint, estimateRedeem, formatAmount, formatDuration, nanosToSecs, parseAmount,
} from "./math";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

/** The contract accepts FinalizeBatch this long after `mature_at` (the chain releases the ATOM at the end of that block). */
const FINALIZE_DELAY_SECS = 60;

let chain: Chain;
let session: Session | null = null;
let cfg: ContractConfig | null = null;
let pool: Pool | null = null;
let balAtom = 0n;
let balLiatom = 0n;
let busy = false;
let validatorSet: string[] = [];
let delegations: Delegation[] = [];
let shareTokens: ShareToken[] = [];

// ---- helpers ---------------------------------------------------------------------------------

function toast(text: string, kind: "ok" | "err" = "ok"): void {
  const el = $("toast");
  el.textContent = text;
  el.className = `toast ${kind}`;
  el.hidden = false;
  window.setTimeout(() => (el.hidden = true), kind === "err" ? 9000 : 5000);
}

const supplyEff = () => (pool ? BigInt(pool.liatom_supply) + BigInt(pool.pending_liatom) : 0n);
const poolValue = () => (pool ? BigInt(pool.pool_value) : 0n);
/** "21 d", "4 h", "5 min": days when long, otherwise the largest unit that fits. */
function span(secs: number): string {
  const d = formatDuration(secs);
  if (secs >= 86400) return `${Math.round((secs / 86400) * 10) / 10} ${t("d")}`;
  if (secs >= 3600) return `${d.hours} ${t("h")}`;
  return `${Math.max(1, d.minutes)} ${t("m")}`;
}

function shortAddr(a: string): string {
  return `${a.slice(0, 10)}...${a.slice(-4)}`;
}

// ---- data ------------------------------------------------------------------------------------

async function refreshProtocol(): Promise<void> {
  cfg = await chain.query<ContractConfig>({ config: {} });
  pool = await chain.query<Pool>({ pool: {} });
  const rate = await chain.query<{ rate: string }>({ exchange_rate: {} });

  $("s-rate").textContent = `${Number(rate.rate).toFixed(6)} ATOM`;
  $("s-tvl").textContent = `${formatAmount(poolValue(), 2)} ATOM`;
  $("s-supply").textContent = `${formatAmount(BigInt(pool.liatom_supply), 2)}`;
  $("s-fee").textContent = `${cfg.fee_bps / 100}%`;

  const banner = $("banner");
  banner.hidden = cfg.pause === null;
  if (cfg.pause) banner.textContent = t(cfg.pause === "full" ? "pausedFull" : "pausedDeposits");

  $("unstake-note").textContent = t("unstakeNote", {
    days: span(cfg.unbonding_secs + cfg.epoch_secs),
    epoch: span(cfg.epoch_secs),
    unb: span(cfg.unbonding_secs),
  });
}

async function refreshUser(): Promise<void> {
  if (!session || !cfg) {
    balAtom = balLiatom = 0n;
    delegations = shareTokens = [];
    renderLsm();
    $("bal-atom").textContent = $("bal-liatom").textContent = "-";
    renderClaims([], new Map());
    return;
  }
  balAtom = await chain.balance(session.address, cfg.bond_denom);
  balLiatom = await chain.balance(session.address, cfg.liatom_denom);
  $("bal-atom").textContent = formatAmount(balAtom);
  $("bal-liatom").textContent = formatAmount(balLiatom);

  try {
    validatorSet = await chain.validatorSet();
    delegations = await chain.delegations(session.address);
    shareTokens = await chain.shareTokens(session.address);
  } catch {
    delegations = shareTokens = [];
  }
  renderLsm();

  const batches = await chain.allBatches();
  const mine = new Map<number, bigint>();
  for (const b of batches) {
    const amount = await chain.userClaim(b.id, session.address);
    if (amount > 0n) mine.set(b.id, amount);
  }
  renderClaims(batches.filter((b) => mine.has(b.id)), mine);
}

async function refreshAll(): Promise<void> {
  try {
    await refreshProtocol();
    await refreshUser();
  } catch (e) {
    toast(String((e as Error).message ?? e), "err");
  }
  renderForms();
}

// ---- forms -----------------------------------------------------------------------------------

function renderForms(): void {
  // stake
  const stakeAmt = parseAmount((($("in-stake") as HTMLInputElement).value));
  const stakeBtn = $("btn-stake") as HTMLButtonElement;
  const stakeHint = $("hint-stake");
  stakeHint.textContent = "";
  let ok = false;
  if (stakeAmt !== null && stakeAmt > 0n && pool && cfg) {
    $("out-stake").textContent = formatAmount(estimateMint(stakeAmt, supplyEff(), poolValue()));
    const min = BigInt(cfg.min_deposit);
    if (stakeAmt < min) stakeHint.textContent = t("belowMin", { min: formatAmount(min) });
    else if (poolValue() + stakeAmt > BigInt(cfg.max_tvl)) {
      const cap = BigInt(cfg.max_tvl);
      stakeHint.textContent = t("capReached", { room: formatAmount(cap > poolValue() ? cap - poolValue() : 0n) });
    }
    else if (session && stakeAmt > balAtom) stakeHint.textContent = t("notEnough");
    else ok = true;
  } else {
    $("out-stake").textContent = "0";
  }
  if (!session) stakeHint.textContent = stakeAmt ? t("connectFirst") : "";
  stakeBtn.disabled = busy || !ok || !session || cfg?.pause != null;

  // unstake
  const unAmt = parseAmount((($("in-unstake") as HTMLInputElement).value));
  const unBtn = $("btn-unstake") as HTMLButtonElement;
  const unHint = $("hint-unstake");
  unHint.textContent = "";
  let unOk = false;
  if (unAmt !== null && unAmt > 0n && pool) {
    $("out-unstake").textContent = formatAmount(estimateRedeem(unAmt, supplyEff(), poolValue()));
    if (session && unAmt > balLiatom) unHint.textContent = t("notEnough");
    else unOk = true;
  } else {
    $("out-unstake").textContent = "0";
  }
  if (!session) unHint.textContent = unAmt ? t("connectFirst") : "";
  unBtn.disabled = busy || !unOk || !session || cfg?.pause === "full";
  ($("btn-unstake-shares") as HTMLButtonElement).disabled = busy || !unOk || !session || cfg?.pause === "full";
}

async function submit(label: string, fn: () => Promise<string>): Promise<void> {
  if (busy) return;
  busy = true;
  renderForms();
  try {
    const hash = await fn();
    const link = chain.cfg.explorerTx ? ` ${chain.cfg.explorerTx.replace("{hash}", hash)}` : ` ${hash.slice(0, 10)}...`;
    toast(`${t("sent")}:${link}`, "ok");
  } catch (e) {
    toast(`${t("failed")} (${label}): ${String((e as Error).message ?? e).slice(0, 300)}`, "err");
  } finally {
    busy = false;
    await refreshAll();
  }
}

function onStake(): void {
  const amount = parseAmount(($("in-stake") as HTMLInputElement).value);
  if (!session || !cfg || amount === null) return toast(t("amountInvalid"), "err");
  void submit("stake", async () => {
    const h = await chain.execute(session!, { deposit: {} }, [{ denom: cfg!.bond_denom, amount: amount.toString() }]);
    ($("in-stake") as HTMLInputElement).value = "";
    return h;
  });
}

function onUnstake(): void {
  const amount = parseAmount(($("in-unstake") as HTMLInputElement).value);
  if (!session || !cfg || amount === null) return toast(t("amountInvalid"), "err");
  void submit("unstake", async () => {
    const h = await chain.execute(session!, { redeem: {} }, [{ denom: cfg!.liatom_denom, amount: amount.toString() }]);
    ($("in-unstake") as HTMLInputElement).value = "";
    return h;
  });
}

// ---- staked ATOM (LSM) ---------------------------------------------------------------------------

function lsmRow(title: string, sub: string, ...extra: HTMLElement[]): HTMLElement {
  const row = document.createElement("div");
  row.className = "claim";
  const left = document.createElement("div");
  const b = document.createElement("b");
  b.textContent = title;
  const st = document.createElement("div");
  st.className = "st";
  st.textContent = sub;
  left.append(b, st);
  row.append(left, ...extra);
  return row;
}

function renderLsm(): void {
  const dList = $("lsm-deleg");
  const tList = $("lsm-tokens");
  dList.replaceChildren();
  tList.replaceChildren();
  const dEmpty = $("lsm-deleg-empty");
  const tEmpty = $("lsm-tokens-empty");
  dEmpty.hidden = tEmpty.hidden = false;
  if (!session) {
    dEmpty.textContent = tEmpty.textContent = t("lsmConnect");
    return;
  }
  dEmpty.textContent = t("lsmNoDelegations");
  tEmpty.textContent = t("lsmNoTokens");
  dEmpty.hidden = delegations.length > 0;
  tEmpty.hidden = shareTokens.length > 0;
  const paused = cfg?.pause != null;

  for (const d of delegations) {
    const accepted = validatorSet.includes(d.validator);
    const input = document.createElement("input");
    input.value = formatAmount(d.amount).replace(/ /g, "");
    input.className = "lsm-amount";
    const btn = document.createElement("button");
    btn.className = "primary";
    btn.textContent = t("convert");
    btn.disabled = busy || !accepted || paused;
    btn.addEventListener("click", () => {
      const amount = parseAmount(input.value);
      if (amount === null || amount <= 0n || amount > d.amount) return toast(t("amountInvalid"), "err");
      void submit("convert", () => chain.tokenize(session!, d.validator, amount));
    });
    dList.append(
      lsmRow(`${formatAmount(d.amount)} ATOM`, accepted ? shortAddr(d.validator) : `${shortAddr(d.validator)} · ${t("lsmNotAccepted")}`, input, btn),
    );
  }

  for (const s of shareTokens) {
    const accepted = validatorSet.includes(s.validator);
    const btn = document.createElement("button");
    btn.className = "primary";
    btn.textContent = t("stakeToken");
    btn.disabled = busy || !accepted || paused;
    btn.addEventListener("click", () => {
      void submit("stake token", () =>
        chain.execute(session!, { deposit_tokenized_shares: {} }, [{ denom: s.denom, amount: s.amount.toString() }]),
      );
    });
    const unwrap = document.createElement("button");
    unwrap.className = "primary";
    unwrap.textContent = t("unwrap");
    unwrap.disabled = busy;
    unwrap.addEventListener("click", () => void submit("redeem token", () => chain.redeemShares(session!, s.denom, s.amount)));
    const est = pool ? ` · ${t("lsmGet")} ${formatAmount(estimateMint(s.tokens, supplyEff(), poolValue()))} liATOM` : "";
    tList.append(
      lsmRow(`${formatAmount(s.tokens)} ATOM`, `${shortAddr(s.validator)}${accepted ? est : " · " + t("lsmNotAccepted")}`, btn, unwrap),
    );
  }
}

function onUnstakeShares(): void {
  const amount = parseAmount(($("in-unstake") as HTMLInputElement).value);
  if (!session || !cfg || amount === null) return toast(t("amountInvalid"), "err");
  void submit("unstake as shares", async () => {
    const h = await chain.execute(session!, { redeem_to_shares: {} }, [{ denom: cfg!.liatom_denom, amount: amount.toString() }]);
    ($("in-unstake") as HTMLInputElement).value = "";
    return h;
  });
}

// ---- claims ----------------------------------------------------------------------------------

function renderClaims(batches: Batch[], mine: Map<number, bigint>): void {
  const list = $("claims-list");
  list.replaceChildren();
  $("claims-empty").hidden = batches.length > 0;
  const now = Math.floor(Date.now() / 1000);

  for (const b of batches.slice().reverse()) {
    const liatom = mine.get(b.id)!;
    const burned = BigInt(b.liatom_burned);
    let status = "";
    let amount = 0n;
    let action: { label: string; msg: object } | null = null;
    let ready = false;

    if (b.status === "open") {
      amount = estimateRedeem(liatom, supplyEff(), poolValue());
      status = t("statusOpen");
    } else if (b.status === "unbonding") {
      amount = claimPayout(BigInt(b.atom_expected ?? "0"), liatom, burned);
      const left = nanosToSecs(b.mature_at!) + FINALIZE_DELAY_SECS - now;
      if (left > 0) {
        status = t("statusUnbonding", { t: span(left) });
      } else {
        status = t("statusFinalize");
        action = { label: t("finalize"), msg: { finalize_batch: { batch_id: b.id } } };
      }
    } else {
      amount = claimPayout(BigInt(b.atom_received ?? "0"), liatom, burned);
      status = t("statusReady");
      ready = true;
      action = { label: t("claim"), msg: { claim: { batch_id: b.id } } };
    }

    const row = document.createElement("div");
    row.className = "claim";
    const left = document.createElement("div");
    const title = document.createElement("b");
    title.textContent = `${formatAmount(amount)} ATOM`;
    const st = document.createElement("div");
    st.className = ready ? "st ok" : "st";
    st.textContent = `${t("batch")} #${b.id} · ${formatAmount(liatom)} liATOM · ${status}`;
    left.append(title, st);
    row.append(left);
    if (action) {
      const btn = document.createElement("button");
      btn.className = "primary";
      btn.textContent = action.label;
      const msg = action.msg;
      btn.addEventListener("click", () => void submit(action!.label, () => chain.execute(session!, msg)));
      row.append(btn);
    }
    list.append(row);
  }
}

// ---- wallet and wiring -------------------------------------------------------------------------

async function onConnect(): Promise<void> {
  const btn = $("connect") as HTMLButtonElement;
  if (session) {
    session = null;
    btn.textContent = t("connect");
    await refreshAll();
    return;
  }
  btn.disabled = true;
  btn.textContent = t("connecting");
  try {
    session = await chain.connect();
    btn.textContent = `${shortAddr(session.address)} ✕`;
  } catch (e) {
    toast(String((e as Error).message ?? e), "err");
    btn.textContent = t("connect");
  } finally {
    btn.disabled = false;
  }
  await refreshAll();
}

function setTab(name: string): void {
  document.querySelectorAll<HTMLElement>(".tab").forEach((el) => el.classList.toggle("active", el.dataset.tab === name));
  for (const id of ["stake", "unstake", "lsm", "claims"]) $(`tab-${id}`).hidden = id !== name;
}

async function main(): Promise<void> {
  applyStaticText();
  $("connect").textContent = t("connect");
  const appCfg = await loadConfig();
  chain = new Chain(appCfg);
  $("network").textContent = appCfg.chainName;
  // The contract address comes from config.json, which is the one file that decides where funds go:
  // show it (with an explorer link when configured) so that a tampered config is visible to a user who looks.
  {
    const info = $("contract-info");
    info.append(document.createTextNode(`Contract: ${appCfg.contract}`));
    if (appCfg.explorerContract) {
      const a = document.createElement("a");
      a.href = appCfg.explorerContract.replace("{address}", appCfg.contract);
      a.textContent = " (explorer)";
      a.rel = "noopener noreferrer";
      info.append(a);
    }
  }
  if (!appCfg.contract.startsWith("cosmos1")) {
    const banner = $("banner");
    banner.hidden = false;
    banner.textContent = t("notLaunched");
    ($("connect") as HTMLButtonElement).disabled = true;
    return;
  }
  try {
    await chain.init();
  } catch (e) {
    toast(String((e as Error).message ?? e), "err");
    return;
  }

  $("connect").addEventListener("click", () => void onConnect());
  document.querySelectorAll<HTMLElement>(".tab").forEach((el) => el.addEventListener("click", () => setTab(el.dataset.tab!)));
  $("in-stake").addEventListener("input", renderForms);
  $("in-unstake").addEventListener("input", renderForms);
  $("btn-stake").addEventListener("click", onStake);
  $("btn-unstake").addEventListener("click", onUnstake);
  $("btn-unstake-shares").addEventListener("click", onUnstakeShares);
  $("max-stake").addEventListener("click", (ev) => {
    ev.preventDefault();
    const reserve = BigInt(appCfg.gasReserve ?? "1000000");
    ($("in-stake") as HTMLInputElement).value = formatAmount(balAtom > reserve ? balAtom - reserve : 0n).replace(/ /g, "");
    renderForms();
  });
  $("max-unstake").addEventListener("click", (ev) => {
    ev.preventDefault();
    ($("in-unstake") as HTMLInputElement).value = formatAmount(balLiatom).replace(/ /g, "");
    renderForms();
  });

  if (new URLSearchParams(location.search).get("devMnemonic") && import.meta.env.DEV) await onConnect();
  else await refreshAll();
  window.setInterval(() => void refreshAll(), 20_000);
}

void main();
