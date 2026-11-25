import unittest

import keeper

NS = 1_000_000_000
CFG = {"epoch_secs": 100, "unbonding_secs": 600}
POOL0 = {"unclaimed_rewards": "0"}


def batch(i, status, burned=0, mature=None, submitted=None):
    b = {"id": i, "status": status, "liatom_burned": str(burned),
         "mature_at": None if mature is None else str(mature * NS)}
    if submitted is not None:
        b["submitted_at"] = str(submitted * NS)
    return b


def decide(now, batches, pool=POOL0, last_harvest=0, min_harvest=1_000_000, interval=3600):
    return keeper.decide(now, CFG, pool, batches, last_harvest, min_harvest, interval)


class DecideTest(unittest.TestCase):
    def test_idle_when_nothing_to_do(self):
        self.assertEqual(decide(1000, {1: batch(1, "open")}), [])

    def test_submit_first_batch_when_it_has_redemptions(self):
        self.assertEqual(decide(1000, {1: batch(1, "open", burned=5)}), [("submit",)])

    def test_empty_open_batch_is_not_submitted(self):
        self.assertEqual(decide(10**6, {1: batch(1, "open", burned=0)}), [])

    def test_submit_waits_for_the_epoch_after_the_previous_submit(self):
        # batch 1 was submitted at 1000 (matures at 1600); next submit allowed from 1100 (+ margin)
        bs = {1: batch(1, "unbonding", 5, mature=1600), 2: batch(2, "open", burned=3)}
        self.assertEqual(decide(1100, bs), [])
        self.assertEqual(decide(1100 + keeper.EPOCH_MARGIN, bs), [("submit",)])

    def test_finalize_only_after_maturity_plus_margin(self):
        bs = {1: batch(1, "unbonding", 5, mature=1600), 2: batch(2, "open")}
        self.assertEqual(decide(1600 + keeper.MATURITY_MARGIN - 1, bs), [])
        self.assertEqual(decide(1600 + keeper.MATURITY_MARGIN, bs), [("finalize", 1)])

    def test_matured_batches_are_left_alone(self):
        bs = {1: batch(1, "matured", 5, mature=100), 2: batch(2, "open")}
        self.assertEqual(decide(10**6, bs), [])

    def test_harvest_needs_rewards_and_a_passed_interval(self):
        pool = {"unclaimed_rewards": "1100000"}
        bs = {1: batch(1, "open")}
        self.assertEqual(decide(10_000, bs, pool, last_harvest=9_000), [])  # interval
        self.assertEqual(decide(10_000, bs, pool, last_harvest=1_000), [("harvest",)])
        small = {"unclaimed_rewards": "10"}
        self.assertEqual(decide(10_000, bs, small, last_harvest=0), [])  # too little

    def test_everything_in_one_tick_finalizes_first(self):
        pool = {"unclaimed_rewards": "5000000"}
        bs = {1: batch(1, "unbonding", 5, mature=100), 2: batch(2, "open", burned=2)}
        self.assertEqual(decide(10_000, bs, pool),
                         [("finalize", 1), ("submit",), ("harvest",)])

    def test_one_finalize_covers_all_matured_batches(self):
        bs = {1: batch(1, "unbonding", 5, mature=100), 2: batch(2, "unbonding", 5, mature=200),
              3: batch(3, "open")}
        self.assertEqual(decide(10_000, bs), [("finalize", 1)])

    def test_submit_is_scheduled_from_the_recorded_submit_time(self):
        # The admin lowered unbonding_secs while batch 1 was in flight: it matures 1100 after the submit
        # at 1000, but the config now says 600. The recorded submit time keeps the schedule right.
        bs = {1: batch(1, "unbonding", burned=5, mature=2100, submitted=1000), 2: batch(2, "open", burned=7)}
        self.assertEqual(decide(1000 + 100, bs), [])  # the epoch has not passed yet
        self.assertEqual(decide(1000 + 100 + 5, bs), [("submit",)])  # the old rule (mature - config) would wait until 1605

    def test_default_gas_price_is_near_the_hub_floor(self):
        # the feemarket charges gas limit x price: 2uatom (the old default) is about 400 times the floor
        import argparse
        src = open(keeper.__file__, encoding="utf-8").read()
        self.assertIn('default="0.01uatom"', src)

    def test_to_secs(self):
        self.assertEqual(keeper.to_secs("1795358883701035354"), 1795358883)


class ExtraDecisions(unittest.TestCase):
    def test_force_harvest_goes_first_whatever_the_threshold(self):
        cfg = {"epoch_secs": 100, "unbonding_secs": 500}
        pool = {"unclaimed_rewards": "10"}
        batches = {1: {"status": "open", "liatom_burned": "5", "mature_at": None}}
        # Below the threshold and inside the interval: nothing without the flag.
        self.assertEqual(keeper.decide(1000, cfg, pool, batches, 990, 1_000_000, 3600),
                         [("submit",)])
        # After a submit failed for lack of delegations the harvest comes first.
        self.assertEqual(keeper.decide(1000, cfg, pool, batches, 990, 1_000_000, 3600, True),
                         [("harvest",), ("submit",)])
        # With nothing to harvest the flag changes nothing.
        empty = {"unclaimed_rewards": "0"}
        self.assertEqual(keeper.decide(1000, cfg, empty, batches, 990, 1_000_000, 3600, True),
                         [("submit",)])

    def test_unbonding_mismatch_is_reported(self):
        self.assertIsNone(keeper.unbonding_warning(1_814_400, 1_814_400))
        self.assertEqual(keeper.unbonding_warning(1_000_000, 1_814_400)[0], "error")
        level, text = keeper.unbonding_warning(1_814_400, 1_000_000)
        self.assertEqual(level, "error")
        self.assertIn("SHORTER", text)


class PredecessorIsKept(unittest.TestCase):
    def test_matured_predecessor_of_the_open_batch_is_not_dropped(self):
        # Review finding (opus5, haiku5): after the first tick the matured batch before the open one was
        # forgotten, the keeper then thought the epoch had passed and sent a doomed submit every tick.
        class Fake(keeper.Chain):
            def __init__(self, batches):
                self.batches = batches

            def try_smart(self, msg):
                return self.batches.get(msg["batch"]["batch_id"])

        batches = {
            1: {"status": "matured", "liatom_burned": "5", "mature_at": "1000000000000"},
            2: {"status": "open", "liatom_burned": "5", "mature_at": None},
        }
        chain = Fake(batches)
        first_id = 1
        for _ in range(3):
            seen, first_id = chain.unfinished_batches(first_id)
            self.assertEqual(sorted(seen), [1, 2])


class ForceHarvestTrigger(unittest.TestCase):
    def test_the_contract_error_text_triggers_the_harvest(self):
        # Review finding (sonnet5): the chain returns the Display text of the contract error
        # ("batch needs ... harvest first or retry later"), never the variant name.
        text = "batch needs 10900000 uatom but only 10000000 uatom is delegated, harvest first or retry later"
        self.assertTrue("InsufficientDelegations" in text or "harvest first" in text)
        src = open(keeper.__file__).read()
        self.assertIn('"harvest first" in str(e)', src)

if __name__ == "__main__":
    unittest.main()
