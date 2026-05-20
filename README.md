# Strate Protocol

Pendle-style yield-stripping protocol on **Stellar Soroban**. Splits a
yield-bearing asset (v1: Blend bUSDC) into:

- **PT** — Principal Token, redeemable 1:1 for underlying at maturity.
- **YT** — Yield Token, claims the variable yield accrued until maturity.

PT and YT trade on an internal AMM using the Pendle V1 logarithmic
implied-rate curve. Mainnet target: **Q3 2026**.

## Layout

```
strate-protocol/
  contracts/             # six Soroban contracts (cdylib)
    yield-stripping/     # mint PT+YT, redeem, claim yield (Week 1 core)
    amm/                 # PT/SY logarithmic-curve AMM (Week 2)
    oracle/              # TWAP wrapper over Blend bRate (Week 2)
    pt-token/            # SEP-41 PT (Week 1)
    yt-token/            # SEP-41 YT + per-holder yield index (Week 1)
    factory/             # deploys (PT, YT, AMM) per maturity (Week 3)
  packages/
    strate-interfaces/   # Error enum, event schemas, traits
    strate-math/         # WAD fixed-point, exp, ln, rounding
    strate-testutils/    # MockBlend, builders, attacker harnesses
  docs/
    ARCHITECTURE.md
    ADR-001..005
    security/THREAT-MODEL.md, INVARIANTS.md, FUZZ-PLAN.md, AUDIT-CHECKLIST.md
```

## How the math relates to the contracts

`strate-math` is the bedrock. Every WAD multiply, divide, exp, or ln in
the AMM, the yield index update, and the rate-anchor computations
routes through it. The crate is `no_std` and has zero dependencies on
`soroban-sdk`, so it tests on the host instantly and can be reused
across non-contract tooling (subgraphs, simulators).

Rounding direction is **explicit at every call site**: contracts pick
`mul_wad_up` for fees/debt and `mul_wad_down` for assets paid out. This
discipline (ported from Pendle's Solidity reference) prevents the
slow-drift bugs that plague AMMs.

## Running tests

```bash
cargo test                       # all tests
cargo test --package strate-math # the math suite specifically
cargo test --release             # release-mode (catches overflow_checks)
```

The math crate ships with:
- 20 differential test vectors (`tests/math_vectors.rs`)
- proptest invariants at 10K cases (`tests/property_tests.rs`)

## Building the contracts

```bash
cargo build --release --target wasm32-unknown-unknown
```

The release profile is tuned for Soroban deployment: `opt-level = z`,
LTO, single codegen unit, panic=abort, overflow checks on. See
`Cargo.toml` for the full profile.

## 8-week build schedule (Q3 2026 mainnet)

| Week | Milestone |
|------|-----------|
| W1   | Workspace, math, interfaces, yield-stripping core, PT, YT (current) |
| W2   | AMM (curve + swaps), Oracle (TWAP ring buffer) |
| W3   | Factory + multi-market integration tests |
| W4   | Threat-model regression suite, fuzz harnesses (FUZZ-PLAN.md) |
| W5   | Frontend SDK + subgraph indexer |
| W6   | Testnet deployment, gas profiling, public testnet incentive run |
| W7   | External audit fixes + final freeze |
| W8   | Mainnet deployment, multisig handover, monitoring activation |

## Status

**Day 3 of W1.** The math library is feature-complete and tested. The
five contracts beyond `yield-stripping` are stubbed with `TODO` and
will be filled in over the next two weeks. The testutils crate has the
attacker harness shapes locked in (one per threat-model entry) waiting
on the contract clients.

## Pre-read

- `docs/ARCHITECTURE.md` — 5-contract design and AMM curve math.
- `docs/security/THREAT-MODEL.md` — the three critical bugs the design
  defends against (C-01 donation, C-02 stale index, C-03 asymptote).
- `docs/ADR-001-cross-contract-call-protocol.md` — how the contracts
  call each other safely.
- Reference: Pendle V1 `PMath.sol` rounding table; PRBMath UD60x18 for
  the exp/ln porting lineage.

## License

BUSL-1.1 — converts to MIT after the 2-year sunset.
