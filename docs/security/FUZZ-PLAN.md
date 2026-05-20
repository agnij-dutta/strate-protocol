# Strate — Fuzz Plan

Each attack class is fuzzed with `proptest = "1.4"`. The plan below names
the strategy, the witness predicate (what state proves the attack
succeeded), the assertion that proves mitigation, and the corpus / iteration
budget. CI runs the **fast tier** on every PR (10K iterations total budget);
the **deep tier** runs nightly with 1M iterations split across vectors.

## Conventions

- **Setup** lists the initial market state in builder syntax (see
  `strate-testutils::builders`).
- **Attacker actions** are a sequence of contract calls; fuzzed inputs are
  marked with `f(<strategy>)`.
- **Witness** is the post-state predicate that, if true, indicates a
  successful exploit.
- **Coverage** lists proptest iteration count and any seed corpus (regression
  fixtures saved in `tests/fixtures/`).

---

## C-01 — Donation inflation (first depositor)

**Setup**:
```rust
Market::builder()
  .pt_supply(0)
  .yt_supply(0)
  .underlying_locked(0)
  .now(maturity - 90_days)
  .build()
```

**Attacker actions**:
1. `attacker.mint(amount = 1)`              // 1 wei
2. `attacker.transfer_underlying_directly(amount = f(1e6..1e18))`  // donation
3. `victim.mint(amount = f(1e6..1e9))`
4. `attacker.redeem_pt_or_withdraw_via_amm(amount = 1)`

**Witness**:
```
attacker.underlying_balance(post) > attacker.underlying_balance(pre)
  + fair_yield_share(attacker)
```

**Mitigation assertion**:
- Either `victim.pt_balance == victim.underlying_in / pseudo_rate(post)`
  with `pseudo_rate` bounded by `1 + 1/VIRTUAL_OFFSET`, OR
- `attacker.redeem_pt(1)` returns ≤ 1 wei (dead-shares burned).

Pick one mitigation in code; the test asserts whichever is documented.

**Coverage**: 10K iterations fast tier, 200K deep tier. Seed corpus:
`fixtures/c01_donation/` includes the exact magnitudes used in the
Cream Finance and Yearn V1 first-depositor exploits, scaled to bUSDC
decimals (7).

---

## C-02 — Stale yield index at YT mint

**Setup**:
```rust
Market::builder()
  .now(maturity - 60_days)
  .seed_yt(victim_a, 1_000e7)
  .blend_rate(1.05e9)            // 5% accrued since launch
  .yield_index_cached(/* lagged */ 1.04e18)
  .build()
```

**Attacker actions**:
1. `mock_blend.set_rate(1.10e9)`              // off-protocol rate jump
2. `// DO NOT call YS.sync_index() — leave cache stale`
3. `attacker.mint(amount = f(1..1e10))`       // snapshot taken vs cached
4. `attacker.claim()`

**Witness**:
```
attacker.claimable_at_step_4 > 0
```
i.e., the attacker claims yield that accrued during the gap between
`yield_index_cached` and the live `blend_rate`.

**Mitigation assertion**:
After `YS.mint()` the global index advanced to the live rate; attacker's
snapshot equals the live rate; `claim()` returns 0.

**Coverage**: 10K iterations fast, 200K deep. Seed corpus exercises
`(cached, live)` rate gaps from 1 bp through 50% rel.

---

## C-03 — AMM near-maturity asymptote

**Setup**:
```rust
Market::builder()
  .now(maturity - f(0..60_minutes))
  .amm_reserves(pt = 1e9, underlying = 1e9)
  .build()
```

**Attacker actions**:
1. `attacker.swap_pt_for_underlying(pt_in = f(1..1e9), min_out = 1)`

**Witness**:
```
attacker.underlying_out > theoretical_out_at_normal_curve(pt_in, reserves)
  * (1 + 5%)        // 5% drift = pool-draining region
```

**Mitigation assertion**:
For any `now > maturity - 30_minutes` the swap reverts with
`MARKET_LOCKED_NEAR_MATURITY`. For `now <= maturity - 30_minutes` swaps
behave within `MAX_APY_MOVE_PER_SWAP`.

**Coverage**: 5K iterations fast, 100K deep. Seed corpus: `fixtures/c03_amm/`
contains the boundary case `now = maturity - 30_minutes + 1_second` and
`now = maturity - 30_minutes - 1_second`.

---

## H-01 — YT transfer recipient stale snapshot

**Setup**:
```rust
Market::builder()
  .now(maturity - 90_days)
  .seed_yt(alice, f(1..1e10))
  .blend_rate(1.05e9)
  .advance(7_days)              // yield index moves
  .build()
```

**Attacker actions**:
1. `alice.transfer(bob, f(1..alice.yt_balance))`
2. `mock_blend.set_rate(rate + 1%)`
3. `advance(7_days)`
4. `bob.claim()`

**Witness**:
```
bob.claimable_after_step_4 > fair_yield_for_bob(transfer_time..now)
```
where `fair_yield_for_bob` is yield accrued only **after** the transfer.

**Mitigation assertion**:
`bob.claimable_after_step_4 == accrued_in_(transfer_time, now)
 * (bob.yt_balance / yt_supply)` within rounding tolerance (±1 wei).

**Coverage**: 20K iterations fast, 500K deep. This is the most likely real
bug in the codebase and gets the largest budget.

---

## A-01 — Oracle manipulation via Blend reserve spike

**Setup**: market mid-tenor, oracle observing every block.

**Attacker actions**:
1. `mock_blend.set_rate(rate * f(1.5..3.0))`   // 1-block jump
2. `attacker.swap_or_mint()`

**Witness**: attacker realises a price out of band with the pre-spike TWAP.

**Mitigation**: Oracle freezes when `|rate_new - rate_old| / rate_old >
MAX_RATE_DELTA_PER_SAMPLE` (default 1%). Frozen state reverts all entrypoints
that read the index until governance manually un-freezes.

**Coverage**: 10K iterations fast.

---

## A-02 — Sandwich at yield-index reset boundary

**Setup**: market mid-tenor; about to settle a yield index advance.

**Attacker actions**:
1. attacker frontruns: `mint(large)`
2. honest user transaction with yield-index-advancing call
3. attacker backruns: `claim() + burn()`

**Witness**: attacker realises non-zero yield with sub-block holding period.

**Mitigation**: snapshot taken **at** advance, not before; combined with
INV-U1, the attacker's snapshot equals the post-advance index, yielding
zero claimable.

**Coverage**: 5K iterations.

---

## A-03 — Rounding-direction drain (PT/YT round-trip)

**Setup**: any market state with non-zero supply.

**Attacker actions** (loop 1000 times):
1. `attacker.mint(1)`
2. `attacker.redeem(1)` (post-maturity branch) OR
   `attacker.burn_yt(1) + claim() + redeem_pt(1)`

**Witness**:
```
attacker.underlying_balance(post_1000_iterations) > start_balance
```

**Mitigation**: WAD math uses `RoundingMode::Down` for all credit-to-user
paths and `RoundingMode::Up` for all debit-from-user paths. The `strate-math`
crate enforces this via type-tagged wrappers.

**Coverage**: 100K iterations (the loop itself is the work; outer proptest
runs 50 different inputs).

---

## A-04 — Reentrancy via SAC custom transfer hook

**Setup**: underlying is a SEP-41 token with a custom `transfer` hook that
re-enters YS.

**Attacker actions**: deploy a malicious wrapper underlying, register a
market against it, then mint/claim/redeem in patterns that re-enter mid-CEI.

**Witness**: any state read mid-interaction-phase returns a value that
differs from the pre-effects-phase value (formal: contract storage
read-before-interaction is inconsistent).

**Mitigation**: CEI enforced (ADR-001 §5). Reentrancy guard on YS via
`access_control::nonreentrant` or equivalent OZ Stellar primitive.

**Coverage**: 5K iterations.

---

## A-05 — Pause griefing near maturity

**Setup**: market 1 day from maturity; some users have unclaimed yield.

**Attacker actions**: owner key compromised; calls `pause()` and never
unpauses. Yield holders cannot claim before maturity.

**Witness**: `claim()` reverts even after `now > maturity`.

**Mitigation**: `claim()` and `redeem_pt()` are **not gated by pause** when
`now >= maturity`. Only mint/swap are pausable. Documented as
`force_exit_after_maturity`.

**Coverage**: 1K iterations (small state space).

---

## A-06 — TWAP same-block manipulation

**Setup**: AMM with TWAP active.

**Attacker actions**: in one transaction, `swap_a() → swap_b()` such that
`swap_b` reads the TWAP just written by `swap_a`.

**Witness**: `swap_b` executes against a TWAP that includes `swap_a`'s
manipulation.

**Mitigation**: TWAP buffer indexed by block timestamp; reads use the
**previous** sample (`samples[len-2]`) not the just-written one
(`samples[len-1]`).

**Coverage**: 5K iterations.

---

## A-07 — Factory deploys market with `maturity < now`

**Setup**: clean factory.

**Attacker actions**: call `Factory.deploy_market(maturity = now - 1)`.

**Witness**: market is deployed; PT becomes immediately redeemable for
fresh-mint underlying.

**Mitigation**: `Factory.deploy_market` requires `maturity > now + MIN_TENOR`.
`MIN_TENOR = 7 days`.

**Coverage**: 100 iterations (bounds check, not stochastic).

---

## A-08 — Storage TTL expiry on long-dated markets

**Setup**: 1-year-tenor market, no user interaction for 3 months.

**Attacker actions**: wait for storage TTL to lapse, observe contract
becomes unusable.

**Witness**: any entrypoint reverts with `StorageMissing`.

**Mitigation**: every entrypoint extends instance TTL via
`env.storage().instance().extend_ttl()` with a conservative bumper; the YS
exposes a public `bump_ttl()` callable by anyone.

**Coverage**: 1K iterations (deterministic but timing-driven).

---

## A-09 — Integer truncation in `scalar_root` for low-decimal assets

**Setup**: market whose underlying is a 6-decimal token (USDC) vs the 18-WAD
math.

**Attacker actions**: feed proptest-fuzzed `(amount, scalar_root)` pairs and
observe rounding.

**Witness**: cumulative drift over 1000 mint/redeem pairs > 1 wei.

**Mitigation**: WAD lift on entry / native lower on exit; `scalar_root` is
in WAD always.

**Coverage**: 50K iterations.

---

## A-10 — YT transferred to YS contract itself

**Setup**: user A has YT; transfers it to the YS contract's address.

**Attacker actions**: `yt.transfer(ys_address, balance)`; `ys.claim()`.

**Witness**: YS claims yield against itself, double-counts global yield.

**Mitigation**: YT's `_update` hook treats `to == ys_address` as a revert
(`TRANSFER_TO_PROTOCOL_FORBIDDEN`). Alternatively, `settle_user(ys_address)`
is a no-op AND `ys_address` is excluded from `claim()`.

**Coverage**: 100 iterations (deterministic).

---

## Tooling notes

- All fuzz seeds saved on failure under `tests/proptest-regressions/`.
- `proptest!` invocations set `ProptestConfig { cases: CI_TIER, .. }` where
  `CI_TIER` is read from the `STRATE_FUZZ_TIER` env var (`fast` or `deep`).
- Mutation-only deltas (e.g., a single rate jump) use direct strategies; for
  composed sequences use `prop_oneof` over a small op vocabulary and
  `prop::collection::vec(op_strategy, 1..50)`.
- Echidna-equivalent symbolic exec on Soroban is not yet available. Use
  `cargo llvm-cov` to confirm coverage and the proptest corpus to brute-
  force coverage of branch conditions.
