# ADR-005: Upgradability and Emergency Response

## Status
Accepted — 2026-05-19

## Context
Upgradeability is a trust trade. Every upgradeable contract is a contract whose admin can, in principle, replace its logic with one that drains user funds. Every non-upgradeable contract is a contract that cannot be patched in place when a bug is found. The right answer is not blanket — it depends on what state the contract holds and how it can be safely redeployed.

For Strate the question is: when a bug is discovered, which contracts can we fix by deploying a new market and migrating off the old one, and which contracts hold state that cannot reasonably be migrated and therefore need in-place upgrade?

## Decision

### Upgradeability matrix

| Contract | Upgradeable? | Why |
|---|---|---|
| **PT token** | No | Pure SEP-41; bug-for-bug equivalent across versions. Migration = deploy new market. |
| **YT token** | No | Same as PT, plus the `_update` hook target is pinned at construction (ADR-001). |
| **AMM** | No | Holds reserves and LP supply, but liquidity is mobile — LPs can withdraw and redeposit into a new AMM. |
| **Oracle** | No | Stateless except for the TWAP ring; ring is reconstructible in a new Oracle by observing a few blocks. |
| **YieldStripping** | **Yes** | Holds per-user `yield_index` snapshots and `claimable` balances that cannot be migrated without coordinated user action. In-place upgrade is the only practical path to fix YS bugs without forcing every user to claim-and-redeposit. |
| **Factory** | Yes | Registry of every market deployed; not migratable. Factory upgrades are rare but must be possible (e.g., new WASM hashes for v2 markets). |

### YieldStripping upgrade mechanism

YS implements an upgradeable proxy pattern (Soroban's `update_current_contract_wasm` host function, guarded):

```
fn propose_upgrade(new_wasm_hash: BytesN<32>)        // admin, records eta = now + 48h
fn cancel_upgrade()                                  // admin, before execute
fn execute_upgrade()                                 // anyone, after eta, calls
                                                     // env.deployer().update_current_contract_wasm(hash)
```

Constraints enforced in code:
- Only one pending upgrade at a time.
- `new_wasm_hash` must be pre-registered in the Factory's WASM allowlist (so the upgrade target is a known, audited binary, not arbitrary bytecode).
- Storage layout migration is the new WASM's responsibility, run in its `__post_upgrade()` hook gated by a version number.
- Upgrade cannot be executed while the contract is paused — pause-then-upgrade-then-unpause must be three separate timelocked operations, preventing a "panic upgrade" foot-gun.

### Factory upgrade mechanism
Same pattern as YS, with the additional constraint that the Factory's `markets` registry layout is append-only across versions. New Factory versions may add columns to `MarketAddresses` but never remove or reorder.

### Emergency pause

| Action | Authority | Latency | Effect |
|---|---|---|---|
| `pause()` | Admin (multisig 3-of-5) | Instant, single tx | Halts `mint`, `swap`, new LP deposits, new market deployment. |
| `unpause()` | Admin via 48h timelock | 48 hours | Restores normal operation. |
| Upgrade YS | Admin via 48h timelock | 48 hours | Replaces WASM. Cannot execute while paused. |

What remains live during pause:
- `YS.claim()` — users can always collect already-accrued yield.
- `YS.redeem_pt()` — post-maturity PT redemption.
- `YS.settle_user()` — internal snapshot updates.
- SEP-41 transfers on PT and YT — tokens are not pausable; they're just bearer claims.
- AMM withdraws by LPs of their existing position — but no new swaps and no new deposits.
- Factory reads — markets remain queryable.

### No emergency withdraw

There is **no admin `emergency_withdraw()` function** on any contract. The combination of:
1. Pause halts inflows.
2. Claim and redeem stay open during pause.
3. AMM LP withdrawal stays open.

…means users always have a path to exit on their own terms. An admin-controlled withdraw is a rug vector dressed up as a safety feature; we reject it categorically.

### Incident response runbook (referenced, not specified here)

1. **Detect** — monitoring alerts on invariant breaks (e.g., `total_pt_supply != Σ balances`, `yield_index` decreasing, Oracle staleness > N ledgers).
2. **Pause** — multisig signs `pause()` on affected contracts within 15 minutes.
3. **Diagnose** — security partner (Cantina) joins the call; root-cause within 24 hours.
4. **Fix** — either deploy a new market (PT/YT/AMM/Oracle bugs) or schedule a YS upgrade (YS bugs).
5. **Unpause / migrate** — 48h timelock window, with public disclosure.

The full runbook is a separate operational document, not in scope here.

### Bug bounty surface

- **Stellar Community Fund** — $50K pool, scoped to Strate's mainnet contracts.
- **Cantina** — $25K continuous bounty, enrolled on mainnet launch day.
- Out-of-scope: testnet, off-chain UIs, third-party integrations (Blend itself has its own bounty), social-engineering against admin signers.
- Severity bands per Immunefi standard; payouts in USDC.

Enrollment is timed to mainnet launch — earlier exposure on testnet without bounty coverage is acceptable because testnet has no real value at risk.

## Consequences
- **Pro:** Non-upgradeable PT/YT/AMM/Oracle makes those four contracts auditable as final artifacts; auditors don't have to reason about future logic substitution.
- **Pro:** Upgradeable YS gives a real fix path for the highest-state contract.
- **Pro:** No emergency withdraw eliminates the most common rug vector.
- **Pro:** Asymmetric pause + 48h upgrade timelock means a single compromised key cannot drain in under 48 hours.
- **Con:** YS being upgradeable is a permanent trust assumption on the admin multisig. We disclose this prominently in user-facing docs.
- **Con:** Fixing an AMM bug requires deploying a new market and convincing LPs to migrate. Real cost; we accept it as the price of AMM immutability.

## Alternatives Considered
1. **Everything upgradeable.** Rejected — multiplies the admin trust surface fivefold for marginal benefit; PT/YT/AMM/Oracle migrations are tractable via redeployment.
2. **Nothing upgradeable.** Rejected — a YS bug post-mainnet would either freeze user funds or force a coordinated mass-exit migration. Operationally untenable.
3. **Admin emergency withdraw with multisig + timelock.** Rejected — even with safeguards, the existence of the function is itself a permanent rug-shaped threat in the threat model; the asymmetric pause + always-open claim/redeem covers the legitimate use case.
4. **Bounty pre-mainnet.** Rejected — splits researcher attention before the audit-frozen code exists; concentrate effort on the final mainnet build.
