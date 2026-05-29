# Strate — Protocol Invariants

All invariants are formally stated as predicates over protocol state. Each
has an ID, a category, a stated form in pseudocode, and a pointer to the
Rust test that checks it. Invariants are checked at the boundaries of every
state-changing entrypoint (`#[invariant_check]` wrapper in the contract) and
also as proptest properties in the test crate.

> **Topology note (2026-05-29):** per ADR-001's push-topology amendment, the
> global yield index and the per-user index/accrual buckets live **inside the
> YT contract**, not YieldStripping. `yield_index` and `user_index_snapshot`
> below refer to YT-owned state. YS keeps the global index fresh via
> `YT.push_yield_index()` and reads settled buckets via `YT.drain_accrued()`.
> All invariants below are re-verified against this topology and continue to
> hold; INV-U1/U2 are annotated where the storage owner matters.

Notation:
- `Σ` denotes sum over a set (clarified at use site).
- `now` is `env.ledger().timestamp()`.
- All balances/supplies are `i128` in token-native decimals unless tagged WAD.
- `idx_t` is `yield_index(t)`, the cumulative-yield index at time t (YT-owned).
- `snap_u` is `user_index_snapshot[u]`, the last index at which u settled
  (YT-owned, advanced by YT's local settle on every balance change).

---

## 1. Solvency (always-holds)

### INV-S1 — Co-issuance of PT and YT
```
∀ t. pt_supply(t) == yt_supply(t)
```
Mint and burn are atomic on both tokens via YieldStripping. Direct PT or YT
mint by anyone other than YieldStripping is impossible (constructor-bound
authority).

**Test**: `proptest_supply_equality`.

### INV-S2 — Backing of PT against underlying floor
```
∀ t < maturity. pt_supply(t) <= underlying_locked(t) / exchange_rate_floor
where exchange_rate_floor := initial_exchange_rate (1.0 WAD at market launch)
```
PT is a 1:1 claim on underlying-units (not bUSDC-units) at maturity, so the
supply cannot exceed the floor-rate conversion of locked underlying. This is
the conservative direction; in practice the cap is much higher because the
Blend rate has appreciated.

**Test**: `proptest_pt_backing_floor`.

### INV-S3 — Post-maturity full redemption capacity
```
∀ t >= maturity.
  underlying_balance(YS_contract) >=
    pt_supply(t)
      + unclaimed_yield_total(t)
      - already_redeemed_via_pt(t)
```
At maturity the contract must be able to honor every outstanding PT 1:1 in
underlying and every accrued unclaimed-yield ticket. The accounting
identity is preserved by the mint/burn/claim hooks.

**Test**: `proptest_solvency_at_maturity`.

### INV-S4 — Unclaimed yield is bounded by accrued yield
```
Σ_u unclaimed_yield(u, t) + Σ_u claimed_yield(u, t)
  <= total_yield_accrued_since_inception(t)
```
A user can never claim, even cumulatively, more yield than the underlying
has actually produced. Equivalent statement: the yield-distribution function
is **conservative**.

**Test**: `proptest_yield_conservation`.

## 2. Monotonicity

### INV-M1 — Global yield index is monotone non-decreasing
```
∀ t1 <= t2. yield_index(t1) <= yield_index(t2)
```
Once advanced, the global index never moves backward — even if Blend's
underlying rate is reported lower (which can only happen via an attacker-
controlled flash interaction; Oracle clamps such reports).

**Test**: `proptest_yield_index_monotone`.

### INV-M2 — Cached Blend exchange rate is monotone (modulo delta cap)
```
∀ t1 <= t2.
  cached_b_rate(t2) >= cached_b_rate(t1)
  ∧ cached_b_rate(t2) - cached_b_rate(t1) <= MAX_RATE_DELTA_PER_SAMPLE
```
Oracle freezes if either condition is violated — protects against a
flash-loan-induced Blend rate spike.

**Test**: `proptest_oracle_freshness`, `proptest_oracle_freeze_on_jump`.

### INV-M3 — PT supply is monotone-down outside mint
```
For any operation o other than mint:
  pt_supply(post_o) <= pt_supply(pre_o)
```
Only `mint` increases supply; redeem and burn strictly decrease. Symmetric
for YT.

**Test**: `proptest_pt_supply_monotone`.

## 3. No-free-lunch (master invariant)

### INV-NFL — Conservation of value
```
For any sequence S = (op_1, op_2, ..., op_n) executed by actor A starting
with deposit D, and ending state E (after all PT/YT positions settled and
claimed):

  underlying_balance_of(A, E) <= D + fair_yield_share(A, S)

where fair_yield_share(A, S) is the time-weighted integral of
  (yt_balance(A, τ) / yt_supply(τ)) * d(total_yield_accrued)/dτ
over τ ∈ [time A first held YT, time A burnt last YT].
```

This is the **master invariant**. Every other invariant in this document is a
necessary condition for INV-NFL. A successful violation of INV-NFL implies a
fund-draining bug.

**Test**: `proptest_no_free_lunch` (highest-stakes proptest; 100K iterations
in CI).

## 4. AMM invariants

### INV-A1 — Curve invariant non-decreasing modulo fee
```
∀ swap. k(post_swap) >= k(pre_swap) - fee_to_protocol(swap)
where k = the parameterized log-curve invariant
       k(PT, U, t) = PT * U^(1-α(t)) (or equivalent — see strate-math::amm)
```
Fees accrue to the protocol pot; swap-side k must not decrease past
fee withdrawal.

**Test**: `proptest_amm_k_non_decreasing`.

### INV-A2 — Implied APY is continuous across a swap
```
∀ swap. | implied_apy(post_swap) - implied_apy(pre_swap) |
        <= MAX_APY_MOVE_PER_SWAP (config: 50% rel.)
```
A single swap cannot move implied APY by more than 50% relative — this is
a soft circuit-breaker that reverts on large reserve depletion before the
curve hits its asymptote.

**Test**: `proptest_amm_apy_continuity`.

### INV-A3 — Strictly positive reserves
```
∀ t < maturity. reserve_pt(t) > 0 ∧ reserve_underlying(t) > 0
```
The curve is undefined at zero on either axis. AMM swaps that would zero a
reserve must revert with `INSUFFICIENT_LIQUIDITY`.

**Test**: `proptest_amm_positive_reserves`.

### INV-A4 — AMM is locked near maturity
```
∀ t. (maturity - t) < N_BLOCKS_BEFORE_MATURITY ⇒ swap reverts
```
Constant: `N_BLOCKS_BEFORE_MATURITY = 30 minutes`. Drives C-03 mitigation.

**Test**: `attack_near_maturity_amm_is_blocked` and
`proptest_amm_lock_window`.

## 5. Per-user accounting

### INV-U1 — Snapshot is fresh after every balance change
```
For every operation that changes yt_balance(u):
  POST: user_index_snapshot[u] == global_yield_index(POST_TIME)
```
**Push topology (amended):** YT settles `from` and `to` **locally** (reading
its own storage) **before** the balance mutation, on every balance-changing
path (`mint`, `burn`, `transfer`, `transfer_from`). There is no callback into
YS. Freshness of `global_yield_index` is guaranteed because YS calls
`YT.push_yield_index()` before any mint/burn/claim, so the index YT settles
against is current as of POST_TIME. Both touched snapshots then equal that
index. This is the C-02 / H-01 mitigation, and it is strictly stronger than
the superseded callback design: settlement is atomic in-contract with no
inter-contract surface for a reentrant settle to bypass.

**Test**: `proptest_yt_transfer_snapshot`,
`attack_yt_transfer_stale_snapshot_is_mitigated`,
`yt_transfer_recipient_does_not_get_stale_yield`,
`yt_burn_credits_accrued_before_balance_decrement`.

### INV-U2 — Snapshot never exceeds global index
```
∀ u, t. user_index_snapshot[u] <= global_yield_index(t)
```
Corollary of INV-M1 and INV-U1.

**Test**: covered transitively by `proptest_yield_conservation`.

## 6. Lifecycle

### INV-L1 — Maturity is monotone and once-set
```
maturity(t) == maturity(0)  ∀ t
maturity(0) > deploy_time
```
Set in `__constructor`. No setter exists. The factory validates `> now`.

**Test**: `attack_factory_past_maturity`.

### INV-L2 — Pause is reversible only by owner; entrypoints respect it
```
paused == true ⇒ all state-changing entrypoints revert
                  except `emergency_withdraw_lp` (defined in §LP)
```
**Test**: `attack_pause_griefing_near_maturity`.

---

## Mapping back to ADR-001

| ADR-001 rule                                | Enforces       |
|---------------------------------------------|----------------|
| §1 Immutable address binding                | INV-L1         |
| §2 Forbidden call edges (incl. removed YT→YS) | INV-S1, INV-NFL|
| §3 YT-local settlement (push topology)      | INV-U1, INV-NFL|
| §4 Oracle freshness rule                    | INV-M2, INV-A2 |
| §5 CEI ordering                             | INV-S4, INV-NFL|

If any of those structural decisions is violated in implementation, the
corresponding invariant **will** fail in the property test. That is the
intended design coupling.
