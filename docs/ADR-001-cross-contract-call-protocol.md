# ADR-001: Cross-Contract Call Protocol

## Status
Accepted — 2026-05-19

## Context
Strate is composed of five core contracts (YieldStripping, AMM, Oracle, PT token, YT token) plus a Factory. Soroban is synchronous (no async re-entry from external mempools mid-transaction), but cross-contract calls still form a directed graph that must be locked to prevent stale-state exploits, accidental cycles, and ambiguous trust boundaries. The most dangerous failure mode — a YT holder minting against a stale yield index and siphoning yield from existing holders — is structurally prevented only if the call protocol is fixed before the foundational workspace solidifies.

The blockchain-developer agent is wiring the Cargo workspace this week. This ADR locks the boundaries it builds against.

## Decision

### 1. Address binding (immutable per market)
Every contract receives the addresses of its peers at construction (via `__constructor`) and stores them in **instance storage**. There is no setter. The Factory passes the full address tuple at deploy time:

```
YieldStripping(underlying, pt, yt, oracle, amm, maturity, scalar_root)
PT(yield_stripping)            // PT only trusts YS for mint/burn
YT(yield_stripping)            // YT only trusts YS; YT also calls back into YS via _update hook
AMM(pt, yt, oracle, maturity)  // AMM does NOT know YS — it is a pure market venue
Oracle(blend_pool, underlying) // Oracle only reads from Blend, no peer awareness
```

The AMM is **deliberately unaware of YieldStripping**. PT/YT trade as standalone SEP-41 tokens; the AMM is just a venue. This severs an entire class of cross-coupling bugs.

### 2. Call graph (allowed edges only)

```
User ──► YieldStripping ──► PT.mint/burn
                       ──► YT.mint/burn
                       ──► Oracle.sync_index()
                       ──► Underlying (Blend bUSDC) transfer

User ──► AMM ──► Oracle.observe()          (TWAP write)
              ──► PT/YT/Underlying transfers (SEP-41 calls only)

YT  ──► YieldStripping.settle_user(from)   (_update hook)
    ──► YieldStripping.settle_user(to)
```

Forbidden edges: YS → AMM, AMM → YS, PT → anything (PT is dumb), Oracle → anything except Blend.

### 3. YT `_update` hook — the stale-rate guard
On every YT balance change (mint, burn, transfer), the YT contract calls `YieldStripping.settle_user(addr)` for both `from` and `to` **before** the balance mutation completes. `settle_user` reads the current yield index, computes pending yield against the user's stored snapshot, credits it to a claimable balance, and updates the snapshot. This makes yield accounting balance-change-atomic and eliminates the donation/siphon class.

### 4. Oracle freshness rule
- `YieldStripping.sync_index()` is called at the start of every entrypoint that mints, burns, or settles YT. It pulls the live Blend exchange rate, advances the global `yield_index`, and only then proceeds.
- `AMM` calls `Oracle.observe()` at the start of every swap to push a new TWAP sample. Trades use the *previous* TWAP, not the just-written one, to resist single-block manipulation.

### 5. CEI (Checks-Effects-Interactions) at every entrypoint
Soroban is synchronous, so classic re-entrancy is bounded, but token callbacks (SAC, custom SEP-41) and the YT `_update` hook still enable reentrant control flow. Every entrypoint follows:
1. **Checks** — auth, maturity gates, paused flag, input bounds.
2. **Effects** — update `yield_index`, supplies, user snapshots.
3. **Interactions** — external token transfers, PT/YT mints, Oracle writes.

## Critical user flows

### Flow A — Mint PT+YT from underlying
```
User → YS.mint(amount)
  YS: check !paused, now < maturity, amount > 0
  YS → Oracle.sync_index()        # pull Blend rate, advance yield_index
  YS: effects — record snapshot for user, increase supplies
  YS → Underlying.transfer_from(user → YS, amount)
  YS → PT.mint(user, amount)
  YS → YT.mint(user, amount)
       YT → YS.settle_user(0x0)   # mint-from-zero is a no-op snapshot
       YT → YS.settle_user(user)  # initialize snapshot at current index
```

### Flow B — Claim accrued yield
```
User → YS.claim()
  YS: check !paused
  YS → Oracle.sync_index()
  YS → YS.settle_user(user)        # internal — credits pending to claimable
  YS: effects — zero claimable
  YS → Underlying.transfer(user, claimable)
```

### Flow C — Redeem PT at maturity
```
User → YS.redeem_pt(amount)
  YS: check now >= maturity, amount > 0
  YS → Oracle.sync_index()         # final index snapshot
  YS: effects — decrease pt_supply
  YS → PT.burn(user, amount)
  YS → Underlying.transfer(user, amount)   # 1:1 in underlying units
```

### Flow D — Swap PT for underlying via AMM
```
Trader → AMM.swap_pt_for_underlying(pt_in, min_out)
  AMM: check now < maturity, !paused
  AMM → Oracle.observe()           # write new TWAP sample
  AMM: read previous TWAP, run logarithmic curve, compute out
  AMM: effects — update reserves
  AMM → PT.transfer_from(trader → AMM, pt_in)
  AMM → Underlying.transfer(AMM → trader, out)
```

YS is not in flow D's path — by design.

## Consequences
- **Pro:** YT `_update` hook removes the entire stale-rate yield-siphon class. AMM/YS decoupling means an AMM bug cannot corrupt PT/YT supply.
- **Pro:** Forbidden edges are enforceable by static review; auditors can grep for them.
- **Con:** Every YT transfer is more expensive (two extra cross-contract calls). Acceptable on Soroban's fee model.
- **Con:** Oracle becomes a hot dependency; its liveness is a protocol liveness assumption (addressed in ADR-002 oracle migration path).

## Alternatives Considered
1. **YS-mediated swaps** (AMM calls back into YS for accounting). Rejected — couples market venue to accounting and was the root cause of two Pendle-on-EVM near-misses.
2. **Pull-based yield index** (no `_update` hook; users must self-sync before transfers). Rejected — relies on user discipline; the donation/transfer-then-claim exploit re-emerges.
3. **Oracle inside YS** (no separate Oracle contract). Rejected — blocks oracle migration to multi-source in v2 without redeploying YS.
