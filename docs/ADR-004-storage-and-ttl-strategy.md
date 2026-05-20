# ADR-004: Storage and TTL Strategy

## Status
Accepted — 2026-05-19

## Context
Soroban charges rent on stored state and expires entries whose TTL runs out. There are three storage types with different cost/lifetime profiles:

- **Instance** — one bag per contract, low cost, TTL bumped automatically on any contract invocation. Hot config.
- **Persistent** — multi-key, per-key TTL, cheapest write but expires if not bumped. User data.
- **Temporary** — cheapest, short TTL, **archived data is unrecoverable**. Never use for value-bearing state.

A long-dated market (12 months out) needs persistent entries to survive that long. The default persistent TTL is well under 12 months, so the protocol must implement an explicit bump strategy, plus a public TTL-extension entrypoint for cold accounts.

## Decision

### Storage classification by contract

**YieldStripping (YS)**

| Key | Type | Reason |
|---|---|---|
| `config: { underlying, pt, yt, oracle, amm, maturity, factory, admin }` | Instance | Hot, read on every call. |
| `paused: bool`, `protocol_fee_bps`, `fee_recipient` | Instance | Hot, admin-touched. |
| `pending_admin_change: Map<Param, Pending>` | Instance | Timelock proposals. |
| `yield_index: i128` (global) | Instance | Read/written on every sync. |
| `total_pt_supply`, `total_yt_supply` | Instance | Invariants. |
| `user_snapshot[user]: { index_at, claimable }` | Persistent | Per-user, sparse. |

**AMM**

| Key | Type | Reason |
|---|---|---|
| `config: { pt, yt, oracle, maturity, scalar_root, factory, admin }` | Instance | Hot. |
| `reserves: { pt, underlying }`, `last_implied_rate` | Instance | Read on every swap. |
| `swap_fee_bps`, `fee_recipient`, `paused` | Instance | Hot. |
| `lp_supply` | Instance | Invariant. |
| `lp_balance[user]` | Persistent | Per-LP. |

**Oracle**

| Key | Type | Reason |
|---|---|---|
| `config: { blend_pool, underlying, factory }` | Instance | Hot. |
| `observations: RingBuffer<Observation>` (TWAP samples) | Instance | Bounded ring, fits in instance. |
| `last_blend_rate`, `last_sync_ledger` | Instance | Hot. |

**PT (SEP-41)**

| Key | Type | Reason |
|---|---|---|
| `metadata: { name, symbol, decimals, minter, maturity }` | Instance | Hot. |
| `total_supply` | Instance | Invariant. |
| `balance[user]`, `allowance[owner][spender]` | Persistent | SEP-41 standard. |

**YT (SEP-41 + hook)** — identical layout to PT, plus the YS address used by the `_update` hook lives in `metadata`.

**Factory**

| Key | Type | Reason |
|---|---|---|
| `config: { admin, paused, deploy_permission, wasm_hashes: {ys, pt, yt, amm, oracle} }` | Instance | Hot. |
| `markets[underlying][maturity] -> MarketAddresses` | Persistent | Grows over time. |
| `markets_index: Vec<MarketKey>` | Persistent | Enumeration. |

**Temporary storage:** used only for non-value-bearing scratchpads (e.g., a transient batch-settle accumulator). Never for balances, snapshots, supplies, reserves, or registry entries. This is a hard rule: a `temporary` archival of a `balance` entry is silent fund loss.

### TTL bump strategy

Constants (initial; tunable by Factory admin via timelock):

```
EXTENDED_TTL          = 500_000 ledgers  (~30 days @ 5s/ledger)
TTL_BUMP_THRESHOLD    = 100_000 ledgers  (bump if remaining < this)
MAX_TTL               = 5_000_000 ledgers (~9.5 months — Soroban network max)
```

On every state-mutating entrypoint, the contract bumps the TTL of every persistent key it touches:

```
bump(persistent, key, low=TTL_BUMP_THRESHOLD, high=EXTENDED_TTL)
```

The `low/high` form is a no-op when the entry already has more than `low` ledgers remaining, avoiding redundant bumps and their fee.

### Long-dated markets — the 12-month problem

Maturity 12 months out ≈ 6.3M ledgers. `MAX_TTL ≈ 5M ledgers`. A user who mints at t=0 and never touches their position before t=10 months will see their `user_snapshot` expire and be archived around month ~9.5.

Archived persistent entries on Soroban are **restorable** (not lost) by paying a restoration fee. Two mitigations:

1. **Public, permissionless extension entrypoint** on YS, AMM, and Factory:
   ```
   fn extend_ttl(keys: Vec<StorageKey>)
   ```
   Anyone can call it, pays the rent fee, no auth required. Keepers, the protocol DAO, or even the user themselves can trigger it. The entrypoint validates each key belongs to a known schema (no arbitrary key spam) and applies `bump(persistent, key, low=0, high=MAX_TTL)`.

2. **Front-end nudge** — the Strate UI shows a "your position TTL expires in N days, extend now" banner inside the last 30 days of any entry's TTL. Costs are paid by the user but trivially small (<$0.01 in expected Soroban fees).

3. **Auto-bump on natural interaction** — any `claim()`, `transfer`, or partial `redeem` already bumps the snapshot. Only fully passive holders need the extension path.

If a user's snapshot is archived and they don't restore it, their next interaction triggers an **on-the-fly restore** (Soroban handles this transparently if the caller includes the restore footprint). The UX cost is one extra fee bump, never fund loss.

### `extend_market_ttl()` convenience
The Factory exposes `extend_market_ttl(underlying, maturity)` which bulk-bumps the five contracts' instance entries and any aggregate persistent keys. Permissionless, no admin gate. Useful for very-long-dated markets and for keepers running a "keep all live markets alive" cron.

## Consequences
- **Pro:** Hot data in instance storage keeps reads cheap and TTL auto-managed.
- **Pro:** Permissionless TTL extension means no admin can grief users by withholding bumps.
- **Pro:** No temporary storage on value-bearing data eliminates a footgun class entirely.
- **Con:** Persistent bump on every mutation adds a per-call fee. Acceptable; tens of stroops.
- **Con:** Users must understand the "archived, restorable" model. Documented in UX flow; not unique to Strate.

## Alternatives Considered
1. **All persistent, no instance.** Rejected — instance auto-bumps on any call, which is exactly what you want for hot config.
2. **Temporary storage for the TWAP ring buffer.** Rejected — ring needs to survive across calls and across long quiescent periods.
3. **Force users to call `extend()` themselves.** Rejected — bad UX, breaks passive HODLers. Permissionless keeper path is strictly better.
4. **Auto-extend to `MAX_TTL` on every call.** Rejected — overpays rent in the common case; threshold-based bumping is cheaper.
