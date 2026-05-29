# Strate — Consolidated Threat Model (v1)

> Status: 2026-05-19. Companion to ADR-001 (cross-contract call protocol). This
> document consolidates the original 10-attack-vector threat model produced
> during scoping; it does **not** restate every vector verbatim — only the
> ones whose risk profile shifted after ADR-001 locked the call graph, plus
> the four critical bugs that drive the property-based test suite.
>
> Treat this as the **authoritative input** for `FUZZ-PLAN.md` and
> `INVARIANTS.md`. If a vector is named here and not covered there, that is
> a defect in the test plan.

---

## 0. Trust model

| Principal              | Trust              | Notes                                              |
|------------------------|--------------------|----------------------------------------------------|
| Factory deployer       | Honest-but-finite  | Can set initial scalar_root, maturity. Then mute.  |
| YieldStripping owner   | Multisig (2/3)     | Can pause only. Cannot upgrade math, change peers. |
| Blend pool             | Honest, lazy       | Rate is monotone modulo rounding; can be stale.    |
| Oracle                 | In-protocol        | Trusts Blend's `b_rate` only. Can freeze on jumps. |
| AMM                    | Permissionless     | Anyone can swap; LPs are anyone with PT+YT.        |
| PT / YT holders        | Adversarial        | Assume max sophistication, flash-loanable capital. |
| Underlying (bUSDC)     | Honest SAC         | Trusted not to reenter on transfer.                |

The hard adversary is the **flash-borrowed PT/YT whale** with full mempool
visibility on Soroban (no private mempool) and the ability to bundle
operations within a single transaction.

## 1. The four critical bugs anchoring this audit

Each maps 1:1 to a test file:
`contracts/yield-stripping/tests/attacks.rs`.

### C-01 — Donation inflation on first deposit
- **Class**: ERC-4626-style first-depositor exploit, ported to PT/YT.
- **Pre-state**: `pt_supply == 0 && yt_supply == 0`.
- **Vector**: attacker mints 1-wei PT+YT, transfers (donates) a large amount
  of underlying directly into YieldStripping's underlying balance, then waits
  for a legitimate depositor. The legitimate depositor's share calculation
  rounds to zero because the donated balance inflates the implicit
  exchange rate. Attacker redeems 1-wei PT (or, more lethally, runs the AMM
  with skewed reserves) and walks away with the victim's principal.
- **Mitigation lever**: virtual-offset shares (Pendle's approach: add a
  constant `VIRTUAL_SHARES = 1e3` to denominator and `VIRTUAL_ASSETS = 1` to
  numerator at supply calculation) **or** burn the first `DEAD_SHARES = 1e3`
  to the zero address. Pick one; document which.
- **Test**: `attack_donation_inflation_is_mitigated`.

### C-02 — Stale yield index at YT mint
- **Class**: Snapshot/check-window race against an external oracle's lazy
  accrual.
- **Pre-state**: Blend's `b_rate` has been updated off-protocol (a borrower
  repaid, accruing interest) but YieldStripping's cached `yield_index` has
  not yet been advanced.
- **Vector**: attacker calls `mint()`, gets snapshot `index_old`. The mint
  path advances `yield_index` to `index_new`. The attacker immediately calls
  `claim()` against `(index_new - index_old) * yt_balance`, extracting yield
  that accrued **before** their position existed.
- **Mitigation lever**: `YS.mint()` must call `Oracle.sync_index()` **first**
  so the snapshot taken for the new YT holder is `index_new`. ADR-001 §5
  enforces this ordering at the entrypoint level. The test must verify it
  empirically.
- **Test**: `attack_stale_rate_yt_mint_is_mitigated`.

### C-03 — AMM math asymptote near maturity
- **Class**: Numerical instability of a non-product-conserving curve at a
  domain boundary.
- **Pre-state**: `now ∈ [maturity - δ, maturity)` where `δ` is small enough
  that `t = time_to_maturity / scalar_root` is below the curve's safe domain.
- **Vector**: the logarithmic implied-rate curve uses `ln(p / (1-p))` where
  `p` is the proportion of PT in the pool. As `t → 0`, the rate component
  amplifies and slippage protections (`min_out`) are insufficient to prevent
  draining the underlying-side reserves with a small PT input.
- **Mitigation lever**: hard-lock the AMM `N_BLOCKS_BEFORE_MATURITY` (Pendle:
  30 minutes / `MARKET_LOCKED_NEAR_MATURITY` revert). All swaps revert.
  LPs can still withdraw because LP withdraw goes through proportional
  balance share, not the curve.
- **Test**: `attack_near_maturity_amm_is_blocked`.

### H-01 — YT transfer recipient stale snapshot
- **Class**: Per-user accounting drift across token transfers.
- **Pre-state**: User A has YT and an up-to-date per-user yield index.
  User B has zero YT.
- **Vector**: User A transfers YT to User B. If B's per-user index is **not**
  settled before B's balance increments, B inherits A's transferred YT with
  B's ancient (possibly zero / possibly default) index. When the global yield
  index advances by `Δ`, B claims `Δ * yt_balance[B]` — yield that accrued
  before B held any YT. This is the single most-commonly-missed Pendle port
  bug.
- **Mitigation (push topology, ADR-001)**: the yield index and the per-user
  accrual buckets live **inside the YT contract**, not in YieldStripping (YS).
  Every balance-changing path (`mint`, `burn`, `transfer`, `transfer_from`)
  settles **both** `from` and `to` **before** the balance mutates by reading
  YT's own storage — there is no cross-contract call. Mint-from-zero and
  burn-to-zero are settled as no-ops on the zero address. YS pushes the new
  global index to YT on `sync()` and pulls a user's settled bucket via
  `drain_accrued()` on `claim()`.

  This design is a deliberate change from the original spec, which had YT
  call back into `YS.settle_user(addr)` from its `_update` hook. Soroban's
  host **forbids contract re-entry** (`Error(Context, InvalidAction)`: "Contract
  re-entry is not allowed"), so the callback could never execute on-chain. The
  push topology removes the callback entirely while strengthening the
  guarantee — both settlements are now atomic within YT, before any balance
  moves, with no inter-contract surface for a reentrant settle to slip through.
  See `docs/handoff/H-01-reentry-fix.md` for the migration record and PR #2.
- **Tests**: `attack_yt_transfer_stale_snapshot_is_mitigated`,
  `yt_transfer_recipient_does_not_get_stale_yield`,
  `yt_burn_credits_accrued_before_balance_decrement`,
  `yt_self_transfer_is_noop`.

## 2. Surface attacks (10 additional vectors)

These come from the original threat model. Severity unchanged; included here
because the property tests reference them.

| ID    | Vector                                                       | Severity | Test                                   |
|-------|--------------------------------------------------------------|----------|----------------------------------------|
| A-01  | Oracle manipulation via Blend reserve spike                  | High     | `proptest_oracle_freshness`            |
| A-02  | Sandwich at yield-index reset boundary                       | High     | `proptest_yield_conservation`          |
| A-03  | Rounding-direction drain (PT/YT round-trip)                  | High     | `proptest_rounding_against_attacker`   |
| A-04  | Reentrancy via SAC custom transfer hook                      | Medium   | `attack_reentrancy_via_sac`            |
| A-05  | Pause / unpause griefing during maturity                     | Medium   | `attack_pause_griefing_near_maturity`  |
| A-06  | TWAP write-then-read same-block manipulation                 | Medium   | `attack_twap_same_block`               |
| A-07  | Factory-deployed market with `maturity < now`                | Medium   | `attack_factory_past_maturity`         |
| A-08  | Storage TTL expiry on long-dated markets (entry locked out)  | Medium   | `proptest_ttl_extension`               |
| A-09  | Integer truncation in `scalar_root` for low-decimal assets   | Low      | `proptest_scalar_root_truncation`      |
| A-10  | YT transferred to YS contract itself (self-claim loop)       | Low      | `attack_yt_to_ys_self`                 |

## 3. Out of scope

- Blend protocol bugs (we wrap, we don't audit them).
- Stellar SAC underlying token re-entrancy beyond SEP-41 contract.
- Governance keys leaking (covered by multi-sig operational runbook, not code).
- MEV-extraction-without-loss on swaps (sandwich profit at LP expense is a
  fee-tuning question, not a security bug).

## 4. Residual risk acknowledgement

After all four critical mitigations and the AMM lock, residual loss vectors:
1. Blend itself depegs / liquidity dries — protocol still solvent in
   underlying-units, but users may be unable to convert bUSDC to USDC.
2. The Oracle's freeze threshold is set too lax and a 50%-rate-jump passes
   through. Tracked in `AUDIT-CHECKLIST.md` item §Oracle.
3. Soroban-level bugs (storage TTL miscalculation in SDK). Out of our
   control; bug bounty covers it via Stellar SCF.
