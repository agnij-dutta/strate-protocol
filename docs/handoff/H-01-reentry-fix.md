# Handoff: H-01 Soroban re-entry violation

**Status:** Open, blocking ~40 tests.
**Severity:** Architectural. Affects every mint / transfer / burn path on YT.
**Last touched:** commit `a27b885` on `main` of `strate-protocol`.
**Hand-off target:** new agent session, focused on this single fix.

---

## TL;DR

The yield-stripping (YS) and yield-token (YT) contracts implement the
H-01 mitigation pattern from `docs/security/THREAT-MODEL.md` using a
cross-contract callback:

```
YS.mint(user, amount)
  ├─ PT.mint(user, amount)
  └─ YT.mint(user, amount)
        └─ invoke_update_hook → calls back into YS.settle_user(user)   ← BLOCKED
```

Soroban host 22.x explicitly forbids re-entering a contract that is
already on the call stack:

```
[Failed Diagnostic Event] data:"Contract re-entry is not allowed"
```

Every test that mints, transfers, or burns YT panics at this boundary.
The protocol architecture assumes EVM-style re-entry semantics. It does
not work on Soroban as written.

## What's working

Built and verified before hitting this bug:

- Rust 1.88 toolchain installed and pinned in `rust-toolchain.toml`
- `cargo build --workspace` clean (9 crates, 0 errors)
- `cargo test -p strate-math` — 10 passed / 0 failed
- All 5 contract WASMs compile under `wasm32v1-none` via
  `stellar contract build` (sizes 13-29 KB each, all under audit cap)
- `cargo test -p strate-factory --features test-wasm` — 29 passed / 0
  failed (covers deploy ordering, registry, determinism, validation,
  pause, upgrade timelock)
- Workspace total before the bug bit: **190 passed / 40 failed**
- All 40 failures trace to this single re-entry violation

## What's broken

| Crate | Failures | Common root cause |
| --- | --- | --- |
| `yield-stripping` (7 test files) | ~14 | YS calls YT, YT calls back to YS.settle_user |
| `strate-integration-tests` (9 test files) | ~20 | Same; these are the end-to-end flow + attacker harnesses |
| `amm` e2e (4 test files) | ~5 | AMM swaps that route through YS.sync trigger the same callback |
| `strate-oracle` (1 test file) | 1 | Independent — staleness check; debug separately |

## Why it broke

The H-01 mitigation was designed against EVM re-entry semantics where
checks-effects-interactions ordering plus a re-entrancy guard is the
norm. The pattern in `yt-token/src/lib.rs`:

> "settle_user for both from and to BEFORE the balance mutates. This is
> the H-01 mitigation — the bug nearly every Pendle port has shipped
> with at least once."

This requires YT to call back into YS during every YT balance change
(mint, transfer, burn). Soroban's host disallows that re-entry by
default — when contract A is on the call stack and contract B (called
by A) tries to call A again, the host raises `Context, InvalidAction`
and the call stack unwinds.

The protocol was authored without ever being compiled against the
Soroban host. The first time the assumption was tested was today.

## Options

### Option A — Inline settle, suppress YT callback

YS settles the user inline before calling YT.mint. YT.mint takes a flag
(`from_minter: bool`) that skips its own settle callback when called
from the trusted YS address.

**Cost:** Low. Maybe 4-6 hours.
**Risk:** Adds a trust-boundary parameter to YT.mint. If an attacker
can pass `from_minter=true` directly to YT.mint, they skip settlement.
Mitigation: gate the flag on `caller == minter` check inside YT.
**Verdict:** Workable but trades structural cleanliness for parameter
trust, which is exactly the kind of thing audits flag.

### Option B — Move yield index from YS into YT (RECOMMENDED)

YT becomes self-contained for accrual. The per-user yield index +
global accrual move from YS storage into YT storage. YS pushes the new
global rate to YT on every `sync()`. YT.transfer / mint / burn settle
inline by reading their own storage. No cross-contract callback exists.

**Cost:** Medium. 2-4 hours of focused refactor + doc updates.
**Risk:** Bigger surface change. Need to re-verify the 17 invariants in
`docs/security/INVARIANTS.md` still hold.
**Verdict:** Cleanest topology. Aligns the protocol with Soroban call
semantics rather than fighting them. Recommended.

### Option C — Drop H-01 mitigation

Document the YT-transfer-then-claim drain vector as a known issue.
Accept that any YT holder who transfers can be drained by the recipient
claiming stale yield first.

**Cost:** Zero code change.
**Risk:** Critical attacker vector. Not mainnet-acceptable.
**Verdict:** No.

## Recommended path (Option B) — concrete plan

### Step 1 — Move yield index storage

In `contracts/yt-token/src/lib.rs`:

- Add storage keys for `GlobalYieldIndex` (i128, WAD-scaled) and
  `UserYieldIndex(Address)` (i128, WAD-scaled). Mirror the keys
  currently in `contracts/yield-stripping/src/lib.rs`.
- Add storage key `Minter` (Address) — already there as
  `initialize_minter` writes it.
- Add storage key `AccruedYield(Address)` (i128) — the underlying-
  denominated amount that has accrued but not been claimed.

### Step 2 — Add YT entry points

```rust
/// Called by YS on every sync(). Pushes the new global yield-per-YT
/// index from YS. Reverts if caller != minter.
pub fn push_yield_index(env: Env, new_index: i128);

/// Returns the current accrued (but unclaimed) yield for `user`.
/// Read-only; no state change.
pub fn accrued_yield(env: Env, user: Address) -> i128;

/// Caller is `user`. Resets accrued_yield to 0 and returns the value
/// so YS.claim can pay it out. Called from YS during the claim flow.
pub fn drain_accrued(env: Env, user: Address) -> i128;
```

### Step 3 — Rewrite the settle path inside YT

Replace the `invoke_update_hook` cross-contract call with a local
function:

```rust
fn settle_user_local(env: &Env, user: &Address) {
    let global = read_global_yield_index(env);
    let prev   = read_user_yield_index(env, user).unwrap_or(global);
    let bal    = read_balance(env, user);
    // Accrue delta * balance / WAD into the user's accrued bucket.
    let delta  = global.checked_sub(prev).unwrap_or(0);
    let earn   = muldiv_floor(delta, bal, WAD).unwrap_or(0);
    if earn > 0 {
        add_accrued_yield(env, user, earn);
    }
    write_user_yield_index(env, user, global);
}
```

Call `settle_user_local(env, from)` and `settle_user_local(env, to)`
at the top of `mint`, `burn`, and `transfer`, before any balance
mutation. The zero-side rule (mint-from-zero, burn-to-zero) still
applies — skip the zero address.

### Step 4 — Rewrite YS to push instead of pull

In `contracts/yield-stripping/src/lib.rs`:

- `YS.sync()` now computes the new global rate and calls
  `YT.push_yield_index(new_index)` instead of writing its own storage.
- `YS.claim(user)` now calls `YT.drain_accrued(user)` to pull the
  payable amount, then transfers underlying to the user. No
  cross-contract callback from YT to YS in either direction.
- Remove `YS.settle_user`. It no longer exists.
- Remove all `UserYieldIndex` and `GlobalYieldIndex` storage from YS.

### Step 5 — Update interfaces

In `packages/strate-interfaces/src/traits.rs`:

- Remove `settle_user` from any YS trait.
- Add `push_yield_index`, `accrued_yield`, `drain_accrued` to a new
  `IYieldToken` extension trait.

### Step 6 — Re-verify invariants

Walk through every invariant in `docs/security/INVARIANTS.md` and
confirm it still holds in the new topology. Specifically:

- **S2 (Solvency: YT supply matches PT supply)** — unchanged.
- **M3 (Monotonicity of accrued yield)** — verify the per-user
  index update path. The series of `delta = global - prev` must
  always be non-negative; if `global` ever decreases the invariant
  is violated. Confirm `push_yield_index` rejects a decrease.
- **NFL (No free lunch on YT transfer)** — the H-01 mitigation
  reincarnated. The new topology is actually stronger because both
  settlements happen atomically inside YT before the balance moves.
- **L1 (Lifecycle: claim ≤ accrued)** — verify `drain_accrued`
  returns exactly what was previously accrued, atomically zeroing
  the bucket. Two concurrent claim calls in the same tx (impossible
  on Soroban without contracts re-entering) must produce identical
  totals.

### Step 7 — Update the threat model

In `docs/security/THREAT-MODEL.md`, rewrite the H-01 section to
describe the new mitigation pattern. The attacker scenario is
unchanged; the defense topology is different.

### Step 8 — Update ADR-001

`docs/ADR-001-cross-contract-call-protocol.md` currently encodes the
YS↔YT callback as the load-bearing protocol. Add a "Superseded" note
pointing at this handoff, and replace section 3 with the push-only
topology.

### Step 9 — Re-run the test suite

```sh
cargo build --workspace
cargo test --workspace --features strate-factory/test-wasm --no-fail-fast
```

Target: 230 passed / 0 failed. Anything less needs triage before
committing.

### Step 10 — Rebuild WASMs

```sh
stellar contract build --package strate-oracle
stellar contract build --package pt-token
stellar contract build --package yt-token
stellar contract build --package amm
stellar contract build --package yield-stripping
```

Confirm sizes are still under 50 KB each. The YT crate will grow
slightly because it absorbed the index storage; the YS crate will
shrink correspondingly.

## Files in scope

Primary edits:

- `contracts/yield-stripping/src/lib.rs` — remove settle_user, switch
  to push topology, simplify claim
- `contracts/yt-token/src/lib.rs` — absorb yield index storage,
  rewrite mint/transfer/burn to settle locally
- `packages/strate-interfaces/src/traits.rs` — interface surface
- `packages/strate-interfaces/src/errors.rs` — possibly new error
  variants (`OnlyMinter`, `IndexRegression`)

Tests likely needing update (all under `contracts/yield-stripping/tests/`
and `tests/integration/`):

- `donation.rs`, `invariants.rs`, `lifecycle.rs`, `maturity.rs`,
  `pause.rs`, `stale_rate.rs`, `transfer.rs`
- All `flow_*.rs` and `attack_*.rs` in `tests/integration/`

Doc updates:

- `docs/security/THREAT-MODEL.md`
- `docs/security/INVARIANTS.md`
- `docs/ADR-001-cross-contract-call-protocol.md`
- `docs/ARCHITECTURE.md` (cross-contract call diagram)

## Test verification path

Run in order; each gate must pass before moving to the next:

1. `cargo build --workspace` — green
2. `cargo test -p yt-token` — green (new settle path)
3. `cargo test -p yield-stripping --lib` — green (sync push path)
4. `cargo test -p yield-stripping` — green (all 7 e2e files)
5. `cargo test --workspace --features strate-factory/test-wasm --no-fail-fast`
   — 230 passed / 0 failed
6. Property tests with bumped iteration count:
   `PROPTEST_CASES=10000 cargo test -p strate-math`
7. Stellar Expert / explorer dry-run on Futurenet (manual, after
   testnet deploy)

## Risks worth flagging during the refactor

- **Migration to existing markets.** The push topology changes YT
  storage layout. If any markets are already deployed on testnet,
  they will not migrate; redeploy is required. (V1 plan was always
  immutable per-market, so this is consistent with ADR-002.)
- **Caller authentication on `push_yield_index`.** Must check
  `caller == minter`. Forgetting this lets anyone bump the index
  and steal yield. Add a test that fails when called from a
  non-minter address.
- **Decrease guard on `push_yield_index`.** Must check
  `new_index >= global_index`. Forgetting this lets an admin error
  rewind accruals and let recent transfers double-claim.
- **Ordering inside `claim`.** YS must settle the user via YT first,
  then transfer underlying. Reversing produces a TOCTOU window.

## When you're done

1. Run the full verification path above.
2. Commit with message starting `H-01 fix: move yield index from YS
   to YT (push topology)`.
3. Close the tracking issue this handoff references.
4. Tag the commit `v0.1.0-h01-fix` so it's the diff the audit team
   reviews.

## Contact points if you get stuck

- **Architecture questions:** re-read `docs/ADR-001` and the
  "Why no OpenZeppelin dependency?" doc block at the top of
  `contracts/pt-token/src/lib.rs`. The first-party SEP-41 there is
  the template for the new YT.
- **Soroban host semantics:** the rs-soroban-env source at
  `~/.cargo/registry/src/index.crates.io-*/soroban-env-host-22.1.3/`
  is the source of truth. Search for `InvalidAction` to see every
  case where the host refuses.
- **Test fixtures:** `contracts/factory/tests/common/mod.rs` is a
  good template for how to wire WASMs and mock auth in tests.

Good luck.
