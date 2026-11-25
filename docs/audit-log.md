# Review log

Thirteen series of independent AI review passes were run on this contract before the repository was published in
its current form. Findings of the earlier series were fixed, covered by tests and by deliberate breakages
(`scripts/dev/mutation-check.sh`); their reports are not kept here. This log lists the latest (13th) series.

| Date | Reviewer | Commit | Scope | Result |
|---|---|---|---|---|
| 2026-11-24 | sonnet (13th series) | `e896e13` | differential testing against an independent reference model | no disagreement that is a contract bug (5,000 cases, every transaction compared); the model is part of the simulator; documentation ambiguities clarified |
| 2026-11-24 | opus (13th series) | `e896e13` | build, keeper, web, supply chain | wasm build not reproducible (fixed), keeper service could not sign and its gas price was far above the network floor (fixed), contract address now shown in the web page, jailed validators accepted by the validator list (documented) |
| 2026-11-24 | haiku (13th series) | `e896e13` | parsing, serialisation, fuzzing | unknown JSON fields were ignored (fixed), unknown protobuf fields were rejected (fixed), unreachable panics in pure functions documented |

If you run a pass of your own, please report it (model, commit, scope, result, including a negative result) as an
issue using the template in [ai-audit.md](ai-audit.md).

## Open items

- `unbonding_secs` should equal the chain's unbonding time. A longer value only slows redemptions (the early ATOM waits
  until `mature_at`); a value shorter than the chain's by more than 7 days makes late ATOM look like slashing. The keeper
  bot logs an error as soon as the two differ; it can be lowered only to 7/8 of its initial value.
- Real slashing, Redelegate limits, multi-validator tokenization in one transaction and the real LSM caps were never
  exercised on a live network. Gas is estimated, not measured (audit-prep.md section 6, item 21).
- No pass by a fourth model family yet.
- The known limits are listed in audit-prep.md section 6.
