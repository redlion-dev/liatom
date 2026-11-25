# Security policy

liATOM is open for review by anyone, with any tools or models (see [docs/ai-audit.md](docs/ai-audit.md)). The first
mainnet launch is capped at 100 ATOM total value (`max_tvl`); the cap is raised only after review passes and time
without incidents. No third-party paid audit has been done: do not treat the absence of findings as proof of safety.

## Reporting a vulnerability

Please report anything that could be exploited **privately**, not in a public issue or pull request. Review notes
that cannot be exploited (a documentation gap, a missing test, a negative result) can still go to public issues using
the template in `docs/ai-audit.md`.

Use GitHub's private vulnerability reporting: open the **Security** tab of this repository and choose
**Report a vulnerability** (<https://github.com/redlion-dev/liatom/security/advisories/new>). Only the maintainers
can see the report.

A useful report has: what is affected (commit, file, function), what an attacker can do, and a failing test or a
numeric trace that reproduces it. A report that says "nothing found" is also welcome (see
[docs/ai-audit.md](docs/ai-audit.md) for the review log format).

## What to expect

- We acknowledge a report and look into it as soon as we can. This is a small project and there is no formal
  response-time commitment.
- We fix confirmed issues, add a regression test, and tell the reporter when the fix is out. We are happy to credit
  you, if you want.
- There is **no bug bounty**.

## Scope

In scope: the `liatom-hub` contract (`contracts/liatom-hub`), the keeper bot (`scripts/keeper`) and the web
interface (`web`).

Out of scope: social engineering, denial of service against infrastructure, issues in third-party software
(Cosmos Hub, CosmWasm, wallets) unless this project's use of it is at fault, and anything that needs a compromised
admin key.

## Please do not

- publish details before a fix is available;
- test against other people's funds or run attacks on mainnet (use a local chain; see
  [docs/dev-environment.md](docs/dev-environment.md));
- access or modify data that is not yours.

## Supported versions

Only the latest commit on the default branch.
