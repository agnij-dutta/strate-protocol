//! Master invariant suite.
//!
//! Runs a randomized sequence of valid operations against a wired
//! market, asserting every INVARIANTS.md invariant after each op. The
//! generator is a simple weighted state machine, not a `prop_compose!`
//! tower — the Soroban `Env` is not `Send`/`Sync` and most proptest
//! combinators want owned values that survive across threads.
//!
//! Iteration tiers:
//!   - default: 32 iterations, 16 ops each (CI fast tier).
//!   - `slow-fuzz` feature: 256 iterations, 64 ops each (CI nightly).
//!
//! The harness does NOT use the `proptest!` macro because each iteration
//! spins up a fresh `Env` and that's incompatible with proptest's
//! single-threaded reruns. We achieve coverage via deterministic LCG
//! seeding instead.

use strate_integration_tests::{accrue, Fixture};
use strate_testutils::constants::{ONE_UNDERLYING, WAD};

/// LCG. Numerical Recipes parameters. Deterministic, fast, sufficient for
/// state-machine variety.
fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state
}

#[derive(Copy, Clone, Debug)]
enum Op {
    MintAlice,
    MintBob,
    ClaimAlice,
    ClaimBob,
    RedeemPairAlice,
    TransferAliceBob(u8), // bps
    AdvanceTime(u32),     // hours
    BlendBump(u8),        // 0..=5 = N * 1%
    OracleSync,
}

fn gen_op(rng: &mut u64) -> Op {
    let r = lcg(rng) % 100;
    match r {
        0..=14 => Op::MintAlice,
        15..=29 => Op::MintBob,
        30..=39 => Op::ClaimAlice,
        40..=49 => Op::ClaimBob,
        50..=54 => Op::RedeemPairAlice,
        55..=64 => Op::TransferAliceBob((lcg(rng) % 100) as u8),
        65..=84 => Op::AdvanceTime((lcg(rng) % 240) as u32 + 1), // 1..240h
        85..=94 => Op::BlendBump((lcg(rng) % 5) as u8 + 1),
        _ => Op::OracleSync,
    }
}

fn apply(f: &Fixture, op: Op) {
    let alice = f.user("alice");
    let bob = f.user("bob");
    match op {
        Op::MintAlice => {
            f.fund(&alice, 100 * ONE_UNDERLYING);
            let _ = f.ys().try_mint(&alice, &alice, &(100 * ONE_UNDERLYING));
        }
        Op::MintBob => {
            f.fund(&bob, 100 * ONE_UNDERLYING);
            let _ = f.ys().try_mint(&bob, &bob, &(100 * ONE_UNDERLYING));
        }
        Op::ClaimAlice => {
            let _ = f.ys().try_claim_yield(&alice);
        }
        Op::ClaimBob => {
            let _ = f.ys().try_claim_yield(&bob);
        }
        Op::RedeemPairAlice => {
            let bal = f.pt_balance(&alice);
            if bal > 0 {
                let half = bal / 2;
                if half > 0 {
                    let _ = f.ys().try_redeem_pair(&alice, &half);
                }
            }
        }
        Op::TransferAliceBob(bps) => {
            let bal = f.yt_balance(&alice);
            let amt = bal * i128::from(bps) / 100;
            if amt > 0 {
                let yt = yt_token::YtTokenClient::new(&f.env, &f.market.yt);
                let _ = yt.try_transfer(&alice, &bob, &amt);
            }
        }
        Op::AdvanceTime(hours) => {
            // Cap the advance so we don't sail through maturity unexpectedly.
            let dt = u64::from(hours) * 3_600;
            let now = f.env.ledger().timestamp();
            // Don't blow past `maturity - 1h` — keep AMM/YS callable.
            let max_target = f.market.maturity.saturating_sub(3_600);
            if now + dt < max_target {
                strate_testutils::time::advance_by(&f.env, dt);
            }
        }
        Op::BlendBump(pct) => {
            accrue(&f, i128::from(pct) * (WAD / 100));
        }
        Op::OracleSync => {
            let _ = f.oracle().try_sync_rate();
            let _ = f.ys().try_sync_yield_index();
        }
    }
}

fn assert_per_op_invariants(f: &Fixture) {
    // INV-S1: pt_supply == yt_supply (always).
    let pt = f.market.pt_supply(&f.env);
    let yt = f.market.yt_supply(&f.env);
    assert_eq!(pt, yt, "INV-S1: pt={pt} yt={yt}");

    // INV-M1: global yield index is non-negative and equal to or above
    // the initial WAD.
    let idx = f.market.yield_index(&f.env);
    assert!(idx >= WAD, "INV-M1 lower-bound: idx={idx}");

    // INV-S2 (conservative form): pt_supply <= YS underlying balance + DEAD.
    let bal = f.market.ys_underlying_balance(&f.env);
    assert!(
        pt <= bal + 1_000,
        "INV-S2: pt={pt} bal={bal}"
    );
}

#[test]
fn master_invariants_fast_tier() {
    let iters: u64 = if cfg!(feature = "slow-fuzz") { 256 } else { 32 };
    let ops_per: u64 = if cfg!(feature = "slow-fuzz") { 64 } else { 16 };

    for seed in 0..iters {
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let f = Fixture::default_market();
        // Establish baseline state.
        let _ = f.oracle().try_sync_rate();
        let _ = f.ys().try_sync_yield_index();
        // Initial mint so subsequent ops have YT to work with.
        let alice = f.user("alice");
        f.fund(&alice, 5_000 * ONE_UNDERLYING);
        let _ = f
            .ys()
            .try_mint(&alice, &alice, &(2_000 * ONE_UNDERLYING));

        for _ in 0..ops_per {
            let op = gen_op(&mut rng);
            apply(&f, op);
            assert_per_op_invariants(&f);
        }

        // Full invariant sweep at the end of the iteration.
        f.assert_invariants();
    }
}
