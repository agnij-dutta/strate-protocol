# ADR-001: Cross-Contract Call Protocol

## Status
Accepted — 2026-05-19. **Amended — 2026-05-29 (push topology, supersedes the
YS↔YT callback).**

> ### Amendment 2026-05-29 — YT-owned yield index (push topology)
>
> The original decision routed yield settlement through a callback: YT's
> `_update` hook called `YieldStripping.settle_user(addr)` on every balance
> change. **This cannot run on Soroban.** The host forbids contract re-entry —
> when YS is mid-call into `YT.mint` and YT calls back into YS, the host aborts
> with `Error(Context, InvalidAction)`: "Contract re-entry is not allowed".
>
> The protocol now uses a **push topology**: the global yield index and the
> per-user accrual buckets live **inside the YT contract**. YT settles users
> locally (reading its own storage) on every balance change — no callback. YS
> **pushes** the new global index to YT via `push_yield_index()` on `sync()`,
> and **pulls** a user's settled bucket via `drain_accrued()` on `claim()`.
> Both calls flow YS → YT only; the reverse edge is gone.
>
> Sections 1–5 and Flows A/B below are annotated inline with the amended
> behaviour. The H-01 guarantee is **strengthened**, not weakened: both
> settlements are now atomic inside YT before any balance moves. See
> `docs/handoff/H-01-reentry-fix.md` and PR #2 for the migration record.

## Context
Strate is composed of five core contracts (YieldStripping, AMM, Oracle, PT token, YT token) plus a Factory. Soroban is synchronous (no async re-entry from external mempools mid-transaction), but cross-contract calls still form a directed graph that must be locked to prevent stale-state exploits, accidental cycles, and ambiguous trust boundaries. The most dangerous failure mode — a YT holder minting against a stale yield index and siphoning yield from existing holders — is structurally prevented only if the call protocol is fixed before the foundational workspace solidifies.

The blockchain-developer agent is wiring the Cargo workspace this week. This ADR locks the boundaries it builds against.

## Decision

### 1. Address binding (immutable per market)
Every contract receives the addresses of its peers at construction (via `__constructor`) and stores them in **instance storage**. There is no setter. The Factory passes the full address tuple at deploy time:

```
YieldStripping(underlying, pt, yt, oracle, amm, maturity, scalar_root)
PT(yield_stripping)            // PT only trusts YS for mint/burn
YT(yield_stripping)            // YT trusts YS for mint/burn + push_yield_index; owns its yield index (amended)
AMM(pt, yt, oracle, maturity)  // AMM does NOT know YS — it is a pure market venue
Oracle(blend_pool, underlying) // Oracle only reads from Blend, no peer awareness
```

The AMM is **deliberately unaware of YieldStripping**. PT/YT trade as standalone SEP-41 tokens; the AMM is just a venue. This severs an entire class of cross-coupling bugs.

### 2. Call graph (allowed edges only)

```
User ──► YieldStripping ──► PT.mint/burn
                       ──► YT.mint/burn
                       ──► YT.push_yield_index()   (on sync — amended)
                       ──► YT.drain_accrued()       (on claim — amended)
                       ──► Oracle.sync_index()
                       ──► Underlying (Blend bUSDC) transfer

User ──► AMM ──► Oracle.observe()          (TWAP write)
              ──► PT/YT/Underlying transfers (SEP-41 calls only)
```

Forbidden edges: YS → AMM, AMM → YS, **YT → YS (removed in the amendment — was
the re-entry violation)**, PT → anything (PT is dumb), Oracle → anything except
Blend. All YS↔YT traffic is now one-directional: YS → YT.

### 3. YT-local settlement — the stale-rate guard (amended)
**Original (superseded):** YT's `_update` hook called
`YieldStripping.settle_user(addr)` for `from` and `to` on every balance change.
The host rejects that re-entry, so it never shipped.

**Current:** the global yield index and per-user accrual buckets live **inside
YT**. On every balance change (`mint`, `burn`, `transfer`, `transfer_from`) YT
settles **both** `from` and `to` **before** the balance mutates, reading its own
storage — no cross-contract call. Settlement computes pending yield against the
user's stored per-user index, credits it to the user's accrued bucket, and
advances the per-user index to the current global value. YS keeps the global
index fresh by calling `YT.push_yield_index(new_index)` at the start of any flow
that mints, burns, or claims; `claim` then calls `YT.drain_accrued(user)` to
pull and zero the settled bucket. This keeps yield accounting balance-change-
atomic and eliminates the donation/siphon class, with strictly fewer trust
edges than the original callback design.

### 4. Oracle freshness rule
- `YieldStripping.sync_index()` is called at the start of every entrypoint that mints, burns, or settles YT. It pulls the live Blend exchange rate, advances the global `yield_index`, and only then proceeds.
- `AMM` calls `Oracle.observe()` at the start of every swap to push a new TWAP sample. Trades use the *previous* TWAP, not the just-written one, to resist single-block manipulation.

### 5. CEI (Checks-Effects-Interactions) at every entrypoint
Soroban is synchronous, so classic re-entrancy is bounded, and the host hard-
rejects contract re-entry outright (the constraint that forced the push
topology). Token callbacks (SAC, custom SEP-41) can still enable reentrant
control flow, so every entrypoint follows:
1. **Checks** — auth, maturity gates, paused flag, input bounds.
2. **Effects** — update `yield_index` (YS pushes to YT), supplies, user buckets.
3. **Interactions** — external token transfers, PT/YT mints, Oracle writes.

## Critical user flows

### Flow A — Mint PT+YT from underlying
```
User → YS.mint(amount)
  YS: check !paused, now < maturity, amount > 0
  YS → Oracle.sync_index()        # pull Blend rate, advance global yield_index
  YS → YT.push_yield_index(idx)   # push fresh global index to YT (amended)
  YS: effects — increase supplies
  YS → Underlying.transfer_from(user → YS, amount)
  YS → PT.mint(user, amount)
  YS → YT.mint(user, amount)
       # YT settles locally, no callback (amended):
       # settle(0x0) is a no-op; settle(user) advances user's per-user index
```

### Flow B — Claim accrued yield
```
User → YS.claim()
  YS: check !paused
  YS → Oracle.sync_index()
  YS → YT.push_yield_index(idx)    # refresh global index in YT (amended)
  YS → YT.drain_accrued(user)      # YT settles user, returns + zeros bucket (amended)
  YS → Underlying.transfer(user, drained)
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
- **Pro:** YT-local settlement removes the entire stale-rate yield-siphon class. AMM/YS decoupling means an AMM bug cannot corrupt PT/YT supply.
- **Pro:** Forbidden edges are enforceable by static review; auditors can grep for them. The amendment removes an edge (YT → YS), shrinking the trust graph.
- **Pro (amended):** YT balance changes settle in-contract with **zero** cross-contract calls, so they are cheaper than the original two-callback design and cannot be blocked by the host's re-entry rule.
- **Con (amended):** YT now owns yield-accounting state, so the YT contract is larger and its storage layout is part of the audited surface. YS must keep the pushed index fresh; a missed `push_yield_index` before a balance change would settle against a stale index (mitigated: every mint/burn/claim path pushes first, covered by tests).
- **Con:** Oracle becomes a hot dependency; its liveness is a protocol liveness assumption (addressed in ADR-002 oracle migration path).

## Alternatives Considered
1. **YS-mediated swaps** (AMM calls back into YS for accounting). Rejected — couples market venue to accounting and was the root cause of two Pendle-on-EVM near-misses.
2. **YT → YS settle_user callback** (the original decision). **Superseded 2026-05-29** — Soroban's host forbids the re-entry this requires. Replaced by the push topology above.
3. **Pull-based yield index** (users must self-sync before transfers). Rejected — relies on user discipline; the donation/transfer-then-claim exploit re-emerges. Note the push topology achieves the callback's safety without this drawback because YT settles automatically on every balance change.
4. **Oracle inside YS** (no separate Oracle contract). Rejected — blocks oracle migration to multi-source in v2 without redeploying YS.
