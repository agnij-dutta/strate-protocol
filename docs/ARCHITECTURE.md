# Strate — Architecture Overview

Strate is a Pendle-style yield-stripping protocol on Stellar Soroban. It splits a yield-bearing asset (v1: Blend bUSDC) into two transferable tokens: **PT** (principal, redeemable 1:1 in underlying at maturity) and **YT** (yield, claims the variable yield accrued until maturity). PT/YT trade on an internal AMM using the Pendle V1 logarithmic curve.

Mainnet target: **Q3 2026**. Build window: **~8 weeks**.

## System diagram

```
                      ┌─────────────────────────────┐
                      │           Factory           │
                      │  deploy_market / registry   │
                      └──────────────┬──────────────┘
                                     │ deploys (per maturity)
              ┌──────────────────────┼──────────────────────┐
              │                      │                      │
              ▼                      ▼                      ▼
  ┌─────────────────────┐  ┌──────────────────┐  ┌─────────────────────┐
  │   YieldStripping    │  │       AMM        │  │       Oracle        │
  │  mint / claim /     │  │  swap_pt_under / │  │  sync_index() /     │
  │  redeem_pt /        │  │  add_liq / rm_liq│  │  observe() / TWAP   │
  │  settle_user /      │  │  (Pendle V1 log) │  │                     │
  │  upgradeable        │  │  immutable       │  │  immutable          │
  └─────┬───────┬───────┘  └────┬─────────┬───┘  └──────────┬──────────┘
        │       │               │         │                 │
        │       │               │         └─── TWAP read ───┤
        │       └─── sync_index ─────────────────────────────┤
        │                       │                            │
        ▼                       ▼                            ▼
  ┌─────────┐ ┌─────────┐  ┌─────────┐                 ┌──────────┐
  │   PT    │ │   YT    │  │ SAC/SEP │                 │  Blend   │
  │ SEP-41  │ │ SEP-41+ │  │   -41   │                 │  Pool    │
  │immutable│ │ _update │  │  wraps  │◄────deposits────┤  bUSDC   │
  └─────────┘ └────┬────┘  │underlyng│                 │ external │
                   │       └─────────┘                 └──────────┘
                   │ settle_user(from), settle_user(to)
                   └────► YieldStripping
```

External dependencies:
- **Blend Protocol** — source of bUSDC and the underlying yield rate.
- **Stellar Asset Contract (SAC)** — SEP-41 wrapper around bUSDC for ergonomic cross-contract transfers.

## Five contracts per market

| Contract | Role | Upgradeable | Admin powers |
|---|---|---|---|
| YieldStripping | Mint PT+YT, sync yield index, settle users, claim, redeem | Yes (timelock) | pause, oracle, fees |
| AMM | Pendle V1 log curve, PT↔underlying swaps, LP | No | pause, fees |
| Oracle | Reads Blend exchange rate, writes TWAP samples | No | pause only |
| PT token | SEP-41, principal claim, mint/burn gated to YS | No | none |
| YT token | SEP-41 + `_update` hook → YS.settle_user | No | none |

Plus a **Factory** (one global) that deploys and registers markets.

## Critical user flows (details in [ADR-001](./ADR-001-cross-contract-call-protocol.md))

1. **Mint PT+YT from underlying** — user deposits Blend bUSDC, receives PT + YT 1:1.
2. **Claim accrued yield** — YT holder collects pending payment in underlying.
3. **Redeem PT at maturity** — PT holder gets underlying back 1:1.
4. **Swap PT for underlying via AMM** — trader exits PT before maturity at a discount.

## Architecture Decision Records

| # | Topic | Summary |
|---|---|---|
| [ADR-001](./ADR-001-cross-contract-call-protocol.md) | Cross-contract call protocol | Locks the call graph; AMM is decoupled from YS; YT `_update` hook prevents stale-rate yield siphoning; CEI at every entrypoint. |
| [ADR-002](./ADR-002-immutability-and-admin-surface.md) | Immutability and admin surface | Per-market immutables (underlying, PT, YT, AMM, maturity, scalar_root); timelocked admin params (oracle, fees); asymmetric pause; 3-of-5 multisig. |
| [ADR-003](./ADR-003-factory-pattern-per-maturity.md) | Factory pattern per maturity | One full `{YS, PT, YT, AMM, Oracle}` per (underlying, maturity); deterministic addresses via salt; admin-gated in v1, permissionless in v2. |
| [ADR-004](./ADR-004-storage-and-ttl-strategy.md) | Storage and TTL strategy | Instance for hot config, persistent for user data, no temporary for value; permissionless TTL extension for long-dated markets. |
| [ADR-005](./ADR-005-upgradability-and-emergency.md) | Upgradability and emergency | Only YS and Factory upgradeable; instant pause, 48h unpause; no emergency withdraw; claim and redeem always open. |

## Key architectural commitments

- **Decoupled AMM** — the AMM has no knowledge of YS. It trades PT/YT/underlying as SEP-41 tokens. An AMM bug cannot corrupt protocol accounting.
- **Snapshot-on-transfer** — YT's `_update` hook settles `from` and `to` before every balance change, eliminating the donation/stale-rate yield siphon at the protocol layer.
- **Per-market immutability** — every market is fully described by its constructor args; no hidden mutable state.
- **Asymmetric emergency response** — pause is instant, unpause is 48 hours, exit paths (claim, redeem) are always open.
- **No admin withdraw** — categorical: a backdoor labelled "safety" is still a backdoor.

## Build coordination

- **blockchain-developer** — implements Cargo workspace, `strate-math` library, and the five contracts to the spec in these ADRs.
- **security-auditor** — owns the threat model and pre-audit checklist; consumes these ADRs as the design oracle.
- **architect (this document)** — owns ADR amendments; any deviation from the call graph or admin surface requires a new ADR or supersession of an existing one.

ADRs are versioned by file. Amendments append a new ADR that explicitly supersedes the prior one (`Status: Superseded by ADR-XXX`). No silent edits to accepted ADRs.
