# Keeper bot

`scripts/keeper/keeper.py` calls the contract's public messages on a schedule: `harvest`, `submit_unbonding`,
`finalize_batch`. The contract does not run itself on a timer; without the bot redemptions are never sent to
unbonding or finalized, and rewards are not reinvested.

All three messages are open to anyone, so the bot's key has no power over funds: it is needed only to pay gas.
Keep a small balance on it (a few ATOM last for months) and use a key separate from any other key.
A stolen bot key cannot withdraw money; the worst case is spent gas.

## Running

Needs `python3` (standard library only, no dependencies) and `gaiad` with a key in its keyring.

```bash
python3 scripts/keeper/keeper.py \
  --contract cosmos1... --chain-id cosmoshub-4 \
  --node https://<rpc> --api https://<rest> \
  --from keeper --keyring-backend os --gas-prices 0.01uatom
```

Defaults: a check every 60 s; `harvest` at most once every 6 hours and only if at least 1 ATOM has accrued
(`--min-harvest`, `--harvest-interval`). `--dry-run` only logs what would be sent; `--once` does one check and exits.

## Logic (`decide` in `keeper.py`, covered by unit tests)

| Action | Condition |
|---|---|
| `finalize_batch` | a batch is `unbonding` and the block time is at least `mature_at` + 75 s (the contract accepts it from + 60 s) |
| `submit_unbonding` | the open batch is not empty and `epoch_secs` have passed since the last submit (computed as the the `submitted_at` the contract records in the previous batch (older batches: `mature_at` minus `unbonding_secs`)) |
| `harvest` | rewards (`unclaimed_rewards`) reach the threshold and the interval has passed |

Every check also compares the chain's `unbonding_time` with the contract's `unbonding_secs` and logs an error when the
contract's value is shorter (the admin must then raise it at once) or longer (the early-arrived ATOM only waits on the balance (redemptions are slower than they need to be); the admin lowers it, down to 7/8 of its initial value). After a `submit_unbonding`
that fails with `InsufficientDelegations` (the chain reports the error text "... harvest first or retry later") (a near-full exit while rewards are unclaimed) the bot harvests first, whatever the
threshold and interval.

Transaction errors do not stop the bot: they are logged and retried at the next check. Typical ones:
`NotReceivedYet` on `finalize_batch` (the funds are still in transit, or real slashing; retried until the contract's
7-day rule accepts a real shortfall), and `EpochNotElapsed` (the bot and the contract count the submit time differently if the admin changed
`unbonding_secs`).

## Checks

```bash
python3 -m unittest discover -s scripts/keeper        # the bot's decisions, no network
bash scripts/dev/keeper-e2e.sh artifacts/liatom_hub.wasm   # on a local chain, about 7 minutes
```

E2E: a user deposits and redeems, then the bot submits the unbonding, harvests and finalizes the batch by itself,
and the user claims the ATOM.

## Running as a service (systemd)

```ini
[Unit]
Description=liATOM keeper
After=network-online.target

[Service]
User=keeper
ExecStart=/usr/bin/python3 /opt/liatom/scripts/keeper/keeper.py --contract cosmos1... --chain-id cosmoshub-4 \
  --node https://<rpc> --api https://<rest> --from keeper --keyring-backend test --home /home/keeper/.gaia
Restart=always
RestartSec=15

[Install]
WantedBy=multi-user.target
```

The keeper key controls nothing (every message it sends is permissionless and carries no funds), so it is kept in `--keyring-backend test` in its own `--home` (mode 600, small balance): a `file` keyring under systemd cannot ask for its passphrase (the process has no stdin) and every transaction would fail. That `--home` must not contain any key that controls funds. Gas price: the Hub charges gas limit x price (`feemarket`), so the default is 0.01uatom, about the network minimum; `--gas auto` takes the limit from the simulation on the `--node`, so run your own node.

Not covered: alerts (the bot only writes a log), several nodes for fault tolerance, a second bot instance (safe:
extra transactions are simply rejected by the contract, but they cost gas).
