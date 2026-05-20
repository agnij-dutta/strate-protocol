# Strate — Pre-Mainnet Self-Audit Checklist

Run top-to-bottom before any mainnet deploy. Each item is a binary
pass/fail. Do not deploy with any unchecked Critical item.

## Math
- [ ] `strate-math` proptests pass at 100K iterations: `STRATE_FUZZ_TIER=deep cargo test -p strate-math`
- [ ] Every WAD math call site annotated with `RoundingMode` — no defaults
- [ ] `mul_wad` / `div_wad` overflow tests cover `i128::MAX / 2 .. i128::MAX`
- [ ] `ln_wad(x)` for `x <= 0` returns `Err`, never panics
- [ ] `exp_wad(x)` domain is documented and proptested against it

## Invariants
- [ ] All `INV-*` properties in `docs/security/INVARIANTS.md` have a matching
      proptest in `contracts/yield-stripping/tests/invariants.rs`
- [ ] `proptest_no_free_lunch` runs at 100K iterations on the deep tier
- [ ] `proptest_supply_equality` proves `pt_supply == yt_supply` at every op
- [ ] AMM `k`-non-decreasing test passes including fee accrual

## Critical bugs (must all show MITIGATED)
- [ ] `attack_donation_inflation_is_mitigated` passes (C-01)
- [ ] `attack_stale_rate_yt_mint_is_mitigated` passes (C-02)
- [ ] `attack_near_maturity_amm_is_blocked` passes (C-03)
- [ ] `attack_yt_transfer_stale_snapshot_is_mitigated` passes (H-01)

## CEI ordering (Soroban has no slither; manual review)
- [ ] Each YS entrypoint: Checks → Effects → Interactions, in that order
- [ ] `Oracle.sync_index()` is **first** call after auth check in every
      entrypoint that touches `yield_index`
- [ ] No external call between an effect and a subsequent state read in the
      same function
- [ ] `nonreentrant` guard on YS public entrypoints
- [ ] AMM swap entrypoint reads TWAP **before** writing the new sample

## Per-user yield index snapshot
- [ ] `YT::_update` hook calls `YS.settle_user(from)` for non-zero `from`
- [ ] `YT::_update` hook calls `YS.settle_user(to)` for non-zero `to`
- [ ] `settle_user` is called for **mint**, **burn**, **transfer**, and
      **transfer_from** (one grep, one test each)
- [ ] Mint-from-zero and burn-to-zero are no-op-but-emitted code paths

## Oracle
- [ ] `MAX_RATE_DELTA_PER_SAMPLE` configured at 1% rel.; freeze test passes
- [ ] Frozen oracle reverts every YS entrypoint until owner un-freezes
- [ ] TWAP read uses `samples[len-2]`, never `samples[len-1]`
- [ ] Oracle staleness check: revert if `last_update < now - MAX_AGE`

## AMM
- [ ] `MARKET_LOCKED_NEAR_MATURITY` triggers `now > maturity - 30_minutes`
- [ ] LP withdraw works post-lock (`withdraw_proportional` path)
- [ ] `MAX_APY_MOVE_PER_SWAP` triggers on out-of-band swaps (50% rel.)
- [ ] Reserve-zero reverts with `INSUFFICIENT_LIQUIDITY`

## Lifecycle
- [ ] `Factory.deploy_market` rejects `maturity < now + MIN_TENOR`
- [ ] All address bindings are constructor-set; no setters exist (grep)
- [ ] Pause is reversible only by owner multi-sig
- [ ] Post-maturity, `claim()` and `redeem_pt()` ignore pause

## Storage / TTL
- [ ] Every entrypoint extends instance TTL
- [ ] Public `bump_ttl()` exists and is callable by anyone
- [ ] Year-long-tenor market simulated for full duration without TTL expiry

## Tooling / coverage
- [ ] `cargo test --workspace` green
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo llvm-cov --workspace --lcov` reports ≥ 95% on contract crates
- [ ] Soroban resource budget profile: every entrypoint < 60% of instruction
      budget at the largest realistic input

## Operational
- [ ] Stellar SCF bug bounty ($50K) enrolled
- [ ] One paid focused review (OtterSec or Cantina, $8-15K) completed
- [ ] All findings ≥ Medium from paid review remediated or formally
      acknowledged in `docs/security/audit-acknowledgements.md`
- [ ] Multi-sig signer set documented (3 keys, 2-of-3 threshold)
- [ ] Incident runbook in `docs/security/incident-runbook.md`
- [ ] Pause key holders rehearsed pause drill end-to-end at least once
