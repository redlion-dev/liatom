#!/usr/bin/env python3
"""liatom-hub keeper bot.

The contract never runs by itself. This bot calls its public maintenance messages on schedule:

  * harvest           when enough rewards piled up and the harvest interval has passed
  * submit_unbonding  when the open batch holds redemptions and the epoch has passed
  * finalize_batch    for every batch whose unbonding period is over

All three are permissionless, so the key only needs gas money and holds no authority. A stolen
keeper key cannot move user funds. Standard library only; transactions are signed by `gaiad`.

  keeper.py --contract cosmos1... --node https://rpc... --api https://rest... \
            --chain-id cosmoshub-4 --from keeper --keyring-backend os
"""
import argparse
import base64
import calendar
import json
import logging
import subprocess
import sys
import time
import urllib.error
import urllib.request

log = logging.getLogger("keeper")

# Seconds to wait past the timestamps in the contract, so block time is surely beyond them.
MATURITY_MARGIN = 75  # the contract accepts FinalizeBatch 60 s after mature_at; add a margin for block time
EPOCH_MARGIN = 5


def to_secs(nanos):
    """The contract serialises Timestamp as a string of nanoseconds."""
    return int(nanos) // 1_000_000_000


def decide(now, cfg, pool, batches, last_harvest, min_harvest, harvest_interval, force_harvest=False):
    """Pure decision function: which transactions should be sent right now.

    cfg      contract Config (epoch_secs, unbonding_secs)
    pool     contract PoolResponse
    batches  {id: Batch} for every batch that is not finished (matured batches may be missing)
    force_harvest  harvest now whatever the threshold and interval (a submit failed for lack of delegations)
    returns  list of ("finalize", id) / ("submit",) / ("harvest",), in the order to send them
    """
    actions = []
    gross = int(pool["unclaimed_rewards"])
    if force_harvest and gross > 0:
        # A near-full exit needs the unclaimed rewards delegated first (InsufficientDelegations).
        actions.append(("harvest",))

    # One finalize settles every matured batch (the contract splits any shortfall pro rata),
    # so a single call is enough; a second one would fail with WrongBatchStatus.
    for bid in sorted(batches):
        b = batches[bid]
        if b["status"] == "unbonding" and now >= to_secs(b["mature_at"]) + MATURITY_MARGIN:
            actions.append(("finalize", bid))
            break

    open_ids = [i for i, b in batches.items() if b["status"] == "open"]
    if open_ids:
        bid = max(open_ids)
        if int(batches[bid]["liatom_burned"]) > 0:
            prev = batches.get(bid - 1)
            # The contract records when a batch was submitted. Older batches did not: they matured
            # unbonding_secs after the submit (which is only right while the config has not changed).
            if prev is not None and prev.get("submitted_at"):
                last_submit = to_secs(prev["submitted_at"])
            elif prev is not None and prev.get("mature_at"):
                last_submit = to_secs(prev["mature_at"]) - int(cfg["unbonding_secs"])
            else:
                last_submit = 0
            if now >= last_submit + int(cfg["epoch_secs"]) + EPOCH_MARGIN:
                actions.append(("submit",))

    if (gross >= min_harvest and now - last_harvest >= harvest_interval
            and ("harvest",) not in actions):
        actions.append(("harvest",))

    return actions


def unbonding_warning(chain_secs, cfg_secs):
    """Compare the chain's unbonding time with the contract's `unbonding_secs`.

    The two must be equal. A longer configured value only delays redemptions. A shorter one makes the
    contract settle a batch before the chain has paid it; beyond the 7-day grace period the late ATOM
    is lost to the redeemers. Returns (level, text) or None when they match.
    """
    if chain_secs == cfg_secs:
        return None
    if cfg_secs > chain_secs:
        return ("error", f"contract unbonding_secs {cfg_secs} is LONGER than the chain's {chain_secs}: "
                         "unbonded ATOM arrives before mature_at and waits on the balance (it can cover another "
                         "batch's shortfall), so redemptions are only slower than they need to be; the admin "
                         "should lower unbonding_secs (down to 7/8 of its initial value)")
    return ("error", f"contract unbonding_secs {cfg_secs} is SHORTER than the chain's {chain_secs} by "
                     f"{chain_secs - cfg_secs} s: batches may be written off before the chain pays them; "
                     "the admin must raise unbonding_secs now")


class Chain:
    def __init__(self, args):
        self.a = args

    def _get(self, path):
        with urllib.request.urlopen(self.a.api.rstrip("/") + path, timeout=20) as r:
            return json.load(r)

    def smart(self, msg):
        q = base64.b64encode(json.dumps(msg).encode()).decode()
        return self._get(f"/cosmwasm/wasm/v1/contract/{self.a.contract}/smart/{q}")["data"]

    def try_smart(self, msg):
        try:
            return self.smart(msg)
        except urllib.error.HTTPError:
            return None

    def unbonding_time(self):
        t = self._get("/cosmos/staking/v1beta1/params")["params"]["unbonding_time"]  # e.g. "1814400s"
        return int(float(t.rstrip("s")))

    def block_time(self):
        t = self._get("/cosmos/base/tendermint/v1beta1/blocks/latest")["block"]["header"]["time"]
        # 2026-11-22T11:52:56.123456789Z
        return calendar.timegm(time.strptime(t[:19], "%Y-%m-%dT%H:%M:%S"))

    def unfinished_batches(self, first_id):
        """Batches from `first_id` on. Returns ({id: batch}, new first_id to remember)."""
        out, bid, new_first = {}, first_id, first_id
        # Keep the batch before the first open one: its mature_at gives the last submit time.
        while True:
            b = self.try_smart({"batch": {"batch_id": bid}})
            if b is None:
                break
            out[bid] = b
            if b["status"] == "matured" and bid == new_first:
                new_first = bid + 1
            bid += 1
        # One matured predecessor is kept for the epoch calculation, and remembered as the id to start
        # from next time (otherwise it is dropped after one tick and the epoch looks elapsed).
        keep_from = max(new_first - 1, 1)
        return {i: b for i, b in out.items() if i >= keep_from}, keep_from

    def execute(self, msg):
        a = self.a
        cmd = [
            a.gaiad, "tx", "wasm", "execute", a.contract, json.dumps(msg),
            "--from", a.key, "--keyring-backend", a.keyring_backend,
            "--chain-id", a.chain_id, "--node", a.node,
            "--gas", "auto", "--gas-adjustment", str(a.gas_adjustment),
            "--gas-prices", a.gas_prices, "-y", "-o", "json", "--broadcast-mode", "sync",
        ]
        if a.home:
            cmd += ["--home", a.home]
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        out = next((l for l in r.stdout.splitlines() + r.stderr.splitlines() if l.startswith("{")), "")
        if not out:
            raise RuntimeError(f"no broadcast result: {(r.stdout + r.stderr).strip()[:300]}")
        res = json.loads(out)
        if res.get("code", 0) != 0:
            raise RuntimeError(f"rejected: {res.get('raw_log', '')[:300]}")
        txhash = res["txhash"]
        for _ in range(30):
            time.sleep(2)
            try:
                tx = self._get(f"/cosmos/tx/v1beta1/txs/{txhash}")["tx_response"]
            except urllib.error.HTTPError:
                continue
            if tx["code"] != 0:
                raise RuntimeError(f"tx {txhash} failed: {tx.get('raw_log', '')[:300]}")
            return txhash
        raise RuntimeError(f"tx {txhash} not included in time")


def run_once(chain, args, state):
    cfg = chain.smart({"config": {}})
    pool = chain.smart({"pool": {}})
    batches, state["first_id"] = chain.unfinished_batches(state["first_id"])
    now = chain.block_time()
    try:
        warn = unbonding_warning(chain.unbonding_time(), int(cfg["unbonding_secs"]))
        if warn:
            (log.error if warn[0] == "error" else log.warning)("%s", warn[1])
    except Exception as e:  # the check is advisory
        log.debug("could not read the chain's unbonding time: %s", e)
    actions = decide(now, cfg, pool, batches, state["last_harvest"],
                     args.min_harvest, args.harvest_interval, state.get("force_harvest", False))
    log.debug("now=%s pool_value=%s gross=%s actions=%s", now, pool["pool_value"],
              int(pool["unclaimed_rewards"]), actions)
    for act in actions:
        msg = {"harvest": {}} if act[0] == "harvest" else \
              {"submit_unbonding": {}} if act[0] == "submit" else \
              {"finalize_batch": {"batch_id": act[1]}}
        if args.dry_run:
            log.info("dry-run: would send %s", json.dumps(msg))
            continue
        try:
            h = chain.execute(msg)
            log.info("sent %s tx=%s", json.dumps(msg), h)
            if act[0] == "harvest":
                state["last_harvest"] = now
                state["force_harvest"] = False
        except Exception as e:  # a failed tx is retried on the next tick
            log.warning("%s failed: %s", json.dumps(msg), e)
            if act[0] == "submit" and ("InsufficientDelegations" in str(e) or "harvest first" in str(e)):
                state["force_harvest"] = True  # harvest first, then the submit goes through
            if act[0] == "harvest":
                state["last_harvest"] = now  # do not hammer a failing harvest every tick


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--contract", required=True)
    p.add_argument("--node", required=True, help="Tendermint RPC, used by gaiad to broadcast")
    p.add_argument("--api", required=True, help="REST API for queries")
    p.add_argument("--chain-id", required=True)
    p.add_argument("--from", dest="key", required=True, help="keyring key name that pays gas")
    p.add_argument("--keyring-backend", default="os")
    p.add_argument("--home", default="")
    p.add_argument("--gaiad", default="gaiad")
    p.add_argument("--gas-prices", default="0.01uatom",
                   help="the Hub feemarket charges gas limit x price, so a high price burns the keeper's balance; "
                        "keep it near the network minimum (check `gaiad q feemarket gas-prices`)")
    p.add_argument("--gas-adjustment", type=float, default=1.6)
    p.add_argument("--interval", type=int, default=60, help="seconds between checks")
    p.add_argument("--min-harvest", type=int, default=1_000_000, help="uatom of rewards that justify a harvest")
    p.add_argument("--harvest-interval", type=int, default=6 * 3600, help="min seconds between harvests")
    p.add_argument("--once", action="store_true", help="run one check and exit")
    p.add_argument("--dry-run", action="store_true", help="only log what would be sent")
    p.add_argument("-v", "--verbose", action="store_true")
    args = p.parse_args(argv)
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(message)s")

    chain = Chain(args)
    state = {"first_id": 1, "last_harvest": 0}
    while True:
        try:
            run_once(chain, args, state)
        except Exception as e:
            log.error("check failed: %s", e)
        if args.once:
            return 0
        time.sleep(args.interval)


if __name__ == "__main__":
    sys.exit(main())
