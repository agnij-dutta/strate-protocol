# ADR-002: Immutability and Admin Surface

## Status
Accepted — 2026-05-19

## Context
Every admin-settable parameter is a potential rug vector and an auditor trust assumption. Conversely, over-aggressive immutability blocks legitimate migrations (oracle v2, fee routing changes) and forces full market redeploys for cosmetic fixes. We need a sharp line: parameters that touch user funds or invariants are immutable per market; parameters that are operational migrations are timelock-gated; emergency response is fast in one direction (pause) and slow in the other (unpause).

This ADR defines the surface every external integrator and auditor will check.

## Decision

### Immutable per market (set in `__constructor`, no setter exists)

| Parameter | Contract | Rationale |
|---|---|---|
| `underlying` | YS, AMM | Defines the asset; changing it is a different market. |
| `pt_token` | YS, AMM | One-to-one with this maturity. |
| `yt_token` | YS | One-to-one with this maturity. |
| `amm` | YS *(reference only — not called)* | Recorded for discoverability. |
| `maturity` | YS, AMM, PT, YT | The defining attribute of the market. |
| `scalar_root` | AMM | Curve parameter; changing it shifts pricing instantly. |
| `factory` | All | For provenance verification. |

These have no setter, no migration path, no governance override. A bug in any of these requires deploying a new market via the Factory and migrating liquidity off-chain.

### Admin-settable with 48-hour timelock

| Parameter | Contract | Bounds |
|---|---|---|
| `oracle` | YS, AMM | Must implement the Oracle trait; rejected if `oracle.underlying() != self.underlying`. |
| `fee_recipient` | YS, AMM | Any non-zero address. |
| `swap_fee_bps` | AMM | 0–100 bps hard cap, enforced in setter. |
| `protocol_fee_bps` | YS | 0–500 bps hard cap (max 5% of yield). |

Timelock flow: `propose_change(param, value)` records `{value, eta = now + 48h}` in instance storage. `execute_change(param)` requires `now >= eta`. A proposal can be cancelled by admin at any time before execution. Only one proposal per param slot at a time.

### Emergency pause — asymmetric

| Action | Latency | Authority |
|---|---|---|
| `pause()` | Instant | Admin (single tx) |
| `unpause()` | 48h timelock | Admin proposal + execute |

While paused: `mint`, `swap`, and all new-deposit entrypoints revert. `claim()`, `redeem_pt()` (post-maturity), and `settle_user()` remain open. Users can always exit; they can never be locked into new exposure.

### Never admin-settable (no setter, no timelock, ever)

- `maturity` — the defining timestamp.
- `total_pt_supply` and `total_yt_supply` — invariants, only mutable via `mint`/`burn`.
- `yield_index` — mutable only via `Oracle.sync_index()` math, not admin.
- User balances and snapshots — only mutable via SEP-41 transfers and `settle_user`.
- The admin address itself is **two-step transferable** (`propose_admin` + `accept_admin`) but not freely settable; the new admin must accept.

### Admin role per contract

| Contract | Admin powers |
|---|---|
| YS | pause/unpause, propose oracle/fee_recipient/protocol_fee_bps, propose_admin |
| AMM | pause/unpause, propose oracle/fee_recipient/swap_fee_bps, propose_admin |
| Oracle | pause/unpause (halts `sync_index`), propose_admin. No parameter setters — Oracle is parameter-free by design. |
| PT, YT | No admin. Mint/burn auth is hardcoded to the YS address from constructor. |
| Factory | propose_admin, set `deploy_permission` (admin-only vs permissionless — see ADR-003). |

PT and YT being admin-less is intentional: SEP-41 tokens with a privileged admin are a known footgun; auditors will look for it and we eliminate it.

### Multisig recommendation
Admin keys are **3-of-5 Stellar multisig**. Suggested signer composition:
- 2 Strate Labs core (founder + protocol lead)
- 1 independent advisor (rotated annually)
- 1 security partner (Cantina or equivalent)
- 1 cold backup (offline hardware, recovery only)

Mainnet launch checklist requires the multisig to be live and the EOA admin path disabled before the Factory is unpaused.

## Consequences
- **Pro:** Per-market immutability means every market is fully describable by its constructor args; no hidden mutable state.
- **Pro:** Asymmetric pause prevents griefing-by-pause (admin cannot indefinitely freeze user exits).
- **Pro:** Timelock + multisig combined means a single compromised key cannot push a change to mainnet within 48h, giving time for community detection.
- **Con:** 48h unpause means a false-positive pause (e.g., from a misfired alert) costs 48h of liquidity. Acceptable — pause should be rare.
- **Con:** Oracle migration is a sensitive operation; the trait-conformance check in the setter must be airtight or it becomes an attack surface itself.

## Alternatives Considered
1. **Fully immutable (no admin at all).** Rejected — oracle migration to multi-source in v2 is a known requirement; locking the v1 oracle forever forces a full liquidity migration on every infrastructure upgrade.
2. **DAO governance from day 1.** Rejected — no token, no quorum reality, and an immature DAO governing a young protocol is governance theater. Revisit post-mainnet with a real governance design ADR.
3. **Single-key admin, no multisig.** Rejected — unacceptable for a yield protocol; single point of failure for $X TVL.
4. **Symmetric instant pause/unpause.** Rejected — symmetric pause gives admin an instant "is it paused right now?" griefing knob. Asymmetric forces commitment.
