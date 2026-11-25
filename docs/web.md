# Web interface

`web/` is a static single-page app (Vite, TypeScript, CosmJS) with no backend. It talks directly to a chain RPC and
to a Keplr or Leap wallet. The interface is English only.

## What it does

| Tab | What it does |
|---|---|
| Stake | `Deposit`: shows how much liATOM you will get (by the contract's formula, rounded down), checks the minimum deposit and the balance; the "Max" button leaves a reserve for gas. Shows a hint when the TVL cap would be exceeded |
| Unstake | `Redeem`: shows the current value of liATOM in ATOM and the withdrawal time (epoch plus the chain's unbonding) |
| Claims | all of the user's batches: "waiting to be sent for unbonding", "unbonding, ready in ...", "needs to be finalized" (a `FinalizeBatch` button), "ready to claim" (a `Claim` button) |

At the top: the rate, total staked, liATOM supply and the fee. If the admin pauses the protocol a banner appears and
the blocked actions are disabled: `deposits_only` disables Stake, Convert and staking a share token, `full` disables Unstake too. Claiming
finished withdrawals is always available.

Amounts are computed in integers (`bigint`) with the same formulas as the contract (`web/src/math.ts`, covered by
tests). The contract decides the real result: the interface only estimates, and the redemption amount is fixed when
the batch is submitted for unbonding.

## Configuration

The file `web/public/config.json` (placed by whoever deploys the interface):

```json
{
  "chainId": "cosmoshub-4",
  "chainName": "Cosmos Hub",
  "rpc": "https://<public or own RPC>",
  "contract": "cosmos1...",
  "bondDenom": "uatom",
  "gasPrice": "0.005uatom",
  "gasReserve": "1000000",
  "explorerTx": "https://www.mintscan.io/cosmos/tx/{hash}"
}
```

Check `gasPrice` against the network's current minimum gas price (the local chain uses `2uatom`). `suggestChain` is
needed only for chains the wallet does not know (the local chain); wallets know Hub mainnet. The RPC node must allow
requests from the interface's domain (CORS) and serve transaction methods.

## Running and building

```bash
cd web
npm install
npm test            # formulas
npm run build       # type check and build into web/dist
npm run dev         # development at http://localhost:5173
```

`web/dist` is static files that any web server can host (nginx, Caddy, GitHub Pages). The bundle is about 2.7 MB
(CosmJS), acceptable for a first launch.

### Checking on a local chain

1. Forward the dev VM's RPC: `ssh -N -L 26657:localhost:26657 <dev-vm>`.
2. Create `web/public/config.local.json` (not tracked by git; `vite dev` reads it instead of `config.json`) with the
   address of a contract deployed on the test chain.
3. Run `npm run dev` and open the page. To test without a wallet extension in `vite dev` mode (not in a build) add
   `?devMnemonic=<test mnemonic>`: signing then uses a local key. Use only local test-chain keys.

Verified by hand on a local chain in a browser (signing with a test key): connecting, staking, a withdrawal request,
batch statuses ("waiting", "unbonding" with a countdown, "ready to claim") and receiving ATOM after the keeper bot
took the batch through unbonding. Real Keplr and Leap wallets and mainnet were not tested.

## Security

- The app does not store or see keys: the wallet signs.
- Every transaction is a `MsgExecuteContract` to the one contract in the config. Make sure only the owner can change
  `config.json` on the host: swapping the contract address swaps the recipient of funds. For mainnet, publish the
  contract address and the interface build hash in several places and restrict access to the hosting.
- The `?devMnemonic=` mode is stripped from the production build (`import.meta.env.DEV`).

### The "Staked ATOM" tab (entering with already staked ATOM)

A user with a delegation moves it into liATOM without unbonding in two steps on one tab: "Convert" tokenizes the delegation (`MsgTokenizeShares` from the `x/liquid` module, signed by the wallet in direct mode; the chain pays out the delegation's rewards first), then "Stake" sends the share token to the contract (`deposit_tokenized_shares`). Only delegations and tokens of validators in the contract's set are offered; the others are marked as not accepted. Checked in a browser against the local chain (signing with a test key): tokenizing 30 ATOM of a 40 ATOM delegation and staking the token raised the TVL from 35 to 65 ATOM. The way back: on the Unstake tab, "Unstake now as staked ATOM" burns liATOM at the current rate and sends a share token (`redeem_to_shares`); on the Staked ATOM tab, "To delegation" redeems the token into the user's own delegation (`MsgRedeemTokensForShares`). Checked in a browser: 5 liATOM became a 5.000028 ATOM token, and the delegation then grew from 10 to 15.000028 ATOM. Hardware wallets in amino mode cannot sign this transaction (the type has no amino converter).

## Not done

APR display, transaction history, charts, mobile wallet support (WalletConnect), localisation of contract errors (the
chain's message is shown as is).
