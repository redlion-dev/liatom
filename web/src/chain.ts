import { CosmWasmClient, SigningCosmWasmClient, wasmTypes } from "@cosmjs/cosmwasm-stargate";
import { DirectSecp256k1HdWallet, Registry, type OfflineSigner } from "@cosmjs/proto-signing";
import {
  defaultRegistryTypes, GasPrice, QueryClient, setupBankExtension, setupStakingExtension,
} from "@cosmjs/stargate";
import { connectComet } from "@cosmjs/tendermint-rpc";
import { t } from "./i18n";
import {
  MsgRedeemTokensForSharesType, MsgTokenizeSharesType, REDEEM_TYPE_URL, parseDec18, shareTokenValidator, sharesToTokens, TOKENIZE_TYPE_URL,
} from "./lsm";

export interface AppConfig {
  chainId: string;
  chainName: string;
  rpc: string;
  contract: string;
  bondDenom: string;
  gasPrice: string;
  /** uatom kept back by "Max" so the wallet can still pay gas. */
  gasReserve?: string;
  /** e.g. "https://www.mintscan.io/cosmos/tx/{hash}" */
  explorerTx?: string;
  /** e.g. "https://www.mintscan.io/cosmos/wasm/contract/{address}" */
  explorerContract?: string;
  /** Keplr chain info, only needed for chains the wallet does not know (local testchain). */
  suggestChain?: Record<string, unknown>;
}

export interface ContractConfig {
  admin: string;
  treasury: string;
  fee_bps: number;
  epoch_secs: number;
  unbonding_secs: number;
  min_deposit: string;
  max_tvl: string;
  pause: null | "deposits_only" | "full";
  liatom_denom: string;
  bond_denom: string;
}

export interface Pool {
  delegated: string;
  unclaimed_rewards: string;
  accrued_fee: string;
  owed_to_redeemers: string;
  pending_liatom: string;
  pool_value: string;
  liatom_supply: string;
}

export interface Batch {
  id: number;
  status: "open" | "unbonding" | "matured";
  liatom_burned: string;
  atom_expected: string | null;
  atom_received: string | null;
  mature_at: string | null;
}

export interface ShareToken {
  denom: string;
  validator: string;
  /** The token amount, which counts delegation shares. */
  amount: bigint;
  /** What the shares are worth in ATOM (they differ after a slash). */
  tokens: bigint;
}

export interface Delegation {
  validator: string;
  amount: bigint;
}

export interface Session {
  address: string;
  client: SigningCosmWasmClient;
}

export async function loadConfig(): Promise<AppConfig> {
  // `vite dev` prefers an untracked config.local.json (points at the local testchain contract).
  const names = import.meta.env.DEV ? ["config.local.json", "config.json"] : ["config.json"];
  for (const name of names) {
    const res = await fetch(`${import.meta.env.BASE_URL}${name}`, { cache: "no-store" });
    if (res.ok && (res.headers.get("content-type") ?? "").includes("json")) return res.json();
  }
  throw new Error("config.json not found");
}

export class Chain {
  private reader!: CosmWasmClient;

  constructor(public cfg: AppConfig) {}

  private rpcUrl(): string {
    return new URL(this.cfg.rpc, window.location.origin).toString();
  }

  async init(): Promise<void> {
    this.reader = await CosmWasmClient.connect(this.rpcUrl());
  }

  query<T>(msg: unknown): Promise<T> {
    return this.reader.queryContractSmart(this.cfg.contract, msg);
  }

  async tryQuery<T>(msg: unknown): Promise<T | null> {
    try {
      return await this.query<T>(msg);
    } catch {
      return null;
    }
  }

  async balance(address: string, denom: string): Promise<bigint> {
    return BigInt((await this.reader.getBalance(address, denom)).amount);
  }

  private lcd?: Promise<QueryClient & ReturnType<typeof setupStakingExtension> & ReturnType<typeof setupBankExtension>>;

  private extended() {
    this.lcd ??= connectComet(this.rpcUrl()).then((c) =>
      QueryClient.withExtensions(c, setupStakingExtension, setupBankExtension),
    );
    return this.lcd;
  }

  /** Validators the contract accepts share tokens of. */
  async validatorSet(): Promise<string[]> {
    const v = await this.query<{ validator: string }[]>({ validators: {} });
    return v.map((w) => w.validator);
  }

  /** The address's delegations (first page; enough for a wallet's handful of validators). */
  async delegations(address: string): Promise<Delegation[]> {
    const q = await this.extended();
    const res = await q.staking.delegatorDelegations(address);
    return res.delegationResponses
      .filter((d) => d.balance && BigInt(d.balance.amount) > 0n)
      .map((d) => ({ validator: d.delegation!.validatorAddress, amount: BigInt(d.balance!.amount) }));
  }

  /** LSM share tokens (`<validator>/<record id>`) in the address's wallet. */
  async shareTokens(address: string): Promise<ShareToken[]> {
    const q = await this.extended();
    const res = await q.bank.allBalances(address);
    const out: ShareToken[] = [];
    const rates = new Map<string, { tokens: bigint; shares: bigint } | null>();
    for (const c of res) {
      const validator = shareTokenValidator(c.denom);
      if (!validator || BigInt(c.amount) <= 0n) continue;
      if (!rates.has(validator)) {
        try {
          const v = (await q.staking.validator(validator)).validator;
          rates.set(validator, v ? { tokens: BigInt(v.tokens), shares: parseDec18(v.delegatorShares) } : null);
        } catch {
          rates.set(validator, null);
        }
      }
      const r = rates.get(validator);
      const amount = BigInt(c.amount);
      out.push({ denom: c.denom, validator, amount, tokens: r ? sharesToTokens(amount, r.tokens, r.shares) : amount });
    }
    return out;
  }

  /** Turns `amount` of the signer's delegation to `validator` into a share token that the signer keeps. */
  async tokenize(s: Session, validator: string, amount: bigint): Promise<string> {
    const res = await s.client.signAndBroadcast(
      s.address,
      [
        {
          typeUrl: TOKENIZE_TYPE_URL,
          value: {
            delegatorAddress: s.address,
            validatorAddress: validator,
            amount: { denom: this.cfg.bondDenom, amount: amount.toString() },
            tokenizedShareOwner: s.address,
          },
        },
      ],
      1.5,
    );
    if (res.code !== 0) throw new Error(res.rawLog ?? "tokenize failed");
    return res.transactionHash;
  }

  /** Turns a share token back into a delegation of the signer (to the token's validator). */
  async redeemShares(s: Session, denom: string, amount: bigint): Promise<string> {
    const res = await s.client.signAndBroadcast(
      s.address,
      [{ typeUrl: REDEEM_TYPE_URL, value: { delegatorAddress: s.address, amount: { denom, amount: amount.toString() } } }],
      1.5,
    );
    if (res.code !== 0) throw new Error(res.rawLog ?? "redeem failed");
    return res.transactionHash;
  }

  /** Every batch from 1 up to the last one that exists. */
  async allBatches(): Promise<Batch[]> {
    const out: Batch[] = [];
    for (let id = 1; ; id++) {
      const b = await this.tryQuery<Batch>({ batch: { batch_id: id } });
      if (!b) return out;
      out.push(b);
    }
  }

  async userClaim(batchId: number, address: string): Promise<bigint> {
    const v = await this.tryQuery<string>({ claim: { batch_id: batchId, address } });
    return BigInt(v ?? "0");
  }

  /** Connects Keplr or Leap. In `vite dev` a `?devMnemonic=` URL parameter signs with a local key instead. */
  async connect(): Promise<Session> {
    let signer: OfflineSigner;
    const dev = import.meta.env.DEV ? new URLSearchParams(location.search).get("devMnemonic") : null;
    if (dev) {
      signer = await DirectSecp256k1HdWallet.fromMnemonic(dev, { prefix: "cosmos" });
    } else {
      const w = window as unknown as Record<string, any>;
      const wallet = w.keplr ?? w.leap;
      if (!wallet) throw new Error(t("walletMissing"));
      if (this.cfg.suggestChain && wallet.experimentalSuggestChain) {
        try {
          await wallet.experimentalSuggestChain(this.cfg.suggestChain);
        } catch {
          /* the chain may already be known to the wallet */
        }
      }
      await wallet.enable(this.cfg.chainId);
      signer = await wallet.getOfflineSignerAuto(this.cfg.chainId);
    }
    const [account] = await signer.getAccounts();
    const registry = new Registry([...defaultRegistryTypes, ...wasmTypes]);
    registry.register(TOKENIZE_TYPE_URL, MsgTokenizeSharesType);
    registry.register(REDEEM_TYPE_URL, MsgRedeemTokensForSharesType);
    const client = await SigningCosmWasmClient.connectWithSigner(this.rpcUrl(), signer, {
      gasPrice: GasPrice.fromString(this.cfg.gasPrice),
      registry,
    });
    return { address: account.address, client };
  }

  /** Executes one contract message and waits for inclusion. Throws with the chain's message on failure. */
  async execute(
    s: Session,
    msg: unknown,
    funds: { denom: string; amount: string }[] = [],
  ): Promise<string> {
    const res = await s.client.execute(s.address, this.cfg.contract, msg as object, 1.5, undefined, funds);
    return res.transactionHash;
  }
}
