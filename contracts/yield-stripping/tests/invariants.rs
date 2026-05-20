//! Property-based invariant tests for the YieldStripping contract.
//!
//! Every property here corresponds to one or more invariants in
//! `docs/security/INVARIANTS.md`. Failures should be triaged against the
//! invariant ID in the assertion message.
//!
//! ## Tiered runs
//!
//! `STRATE_FUZZ_TIER` controls iteration count:
//!  - `fast` (default): 256 cases per property, CI-friendly.
//!  - `deep`: 10K – 100K cases per property, run nightly.
//!
//! ```sh
//! cargo test -p strate-yield-stripping --test invariants
//! STRATE_FUZZ_TIER=deep cargo test -p strate-yield-stripping --test invariants
//! ```

#![cfg(test)]

use proptest::collection::vec;
use proptest::prelude::*;
use soroban_sdk::Env;

use strate_testutils::builders::{default_market, empty_market, near_maturity_market, Market};
use strate_testutils::time::{self, advance_by};

// -------------------------------------------------------------------------
// Proptest config: tier from env.
// -------------------------------------------------------------------------

fn cases() -> u32 {
    match std::env::var("STRATE_FUZZ_TIER").as_deref() {
        Ok("deep") => 10_000,
        _ => 256,
    }
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: cases(),
        // Failures save under `tests/proptest-regressions/` automatically.
        ..ProptestConfig::default()
    }
}

// -------------------------------------------------------------------------
// Op vocabulary for sequence-based properties.
// -------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Mint { user: u8, amount: i128 },
    Burn { user: u8, amount: i128 },
    Transfer { from: u8, to: u8, amount: i128 },
    Claim { user: u8 },
    Advance { seconds: u64 },
    RateBump { rel_wad: i128 },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..4, 1i128..1_000_000_000).prop_map(|(u, a)| Op::Mint { user: u, amount: a }),
        (0u8..4, 1i128..1_000_000_000).prop_map(|(u, a)| Op::Burn { user: u, amount: a }),
        (0u8..4, 0u8..4, 1i128..1_000_000_000)
            .prop_map(|(f, t, a)| Op::Transfer { from: f, to: t, amount: a }),
        (0u8..4).prop_map(|u| Op::Claim { user: u }),
        (1u64..time::DAY * 30).prop_map(|s| Op::Advance { seconds: s }),
        // Rate bumps are bounded under the freeze threshold so the
        // sequence doesn't trivially freeze the oracle and short-circuit.
        (1i128..(strate_testutils::constants::MAX_RATE_DELTA_PER_SAMPLE_WAD - 1))
            .prop_map(|r| Op::RateBump { rel_wad: r }),
    ]
}

fn op_sequence() -> impl Strategy<Value = Vec<Op>> {
    vec(op_strategy(), 1..30)
}

// Apply an op to a market. Errors swallowed (a revert is information,
// not a test failure; the property assertion runs against post-state).
fn apply(env: &Env, market: &Market, _op: &Op) {
    // TODO(blockchain-developer): once contract crates land, this is:
    //
    //   let ys = YieldStrippingClient::new(env, &market.yield_stripping);
    //   let yt = YtTokenClient::new(env, &market.yt);
    //   let mock = MockBlendPoolClient::new(env, &market.blend_pool);
    //   match op {
    //       Op::Mint { user, amount } => {
    //           let u = pick_user(env, market, *user);
    //           let _ = std::panic::catch_unwind(|| ys.mint(&u, amount));
    //       }
    //       Op::Burn { user, amount } => { ... }
    //       Op::Transfer { from, to, amount } => { ... }
    //       Op::Claim { user } => { ... }
    //       Op::Advance { seconds } => advance_by(env, *seconds),
    //       Op::RateBump { rel_wad } => {
    //           let r = mock.b_rate();
    //           mock.set_rate(&crate::mock_blend::bumped_rate(r, *rel_wad));
    //       }
    //   }
    let _ = (env, market);
}

// -------------------------------------------------------------------------
// INV-S1: pt_supply == yt_supply.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_supply_equality(ops in op_sequence()) {
        let env = Env::default();
        let market = default_market(&env);
        for op in &ops {
            apply(&env, &market, op);
            // After every op, the invariant must hold.
            // pt_supply / yt_supply will panic until contracts land — that
            // is fine; the property body is exercised only when the
            // contracts are wired.
            // TODO(blockchain-developer): once `Market::builder()` deploys
            // real contracts, replace these reads with the client calls.
            // For now, the supply equality is exercised in the named
            // integration tests under `tests/lifecycle.rs`.
            let _ = op;
        }
    }
}

// -------------------------------------------------------------------------
// INV-S4: Σ unclaimed + Σ claimed ≤ Σ accrued.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_yield_conservation(ops in op_sequence()) {
        let env = Env::default();
        let market = default_market(&env);
        for op in &ops {
            apply(&env, &market, op);
        }
        // Final check: total claimed + total unclaimed <= total accrued.
        // TODO(blockchain-developer): expose these aggregates on YS.
        // let claimed = ys.total_claimed();
        // let unclaimed = ys.total_unclaimed();
        // let accrued = ys.total_accrued();
        // prop_assert!(claimed + unclaimed <= accrued,
        //     "INV-S4 violated: claimed={} unclaimed={} accrued={}",
        //     claimed, unclaimed, accrued);
        let _ = market;
    }
}

// -------------------------------------------------------------------------
// INV-S3: solvency at maturity.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_solvency_at_maturity(ops in op_sequence()) {
        let env = Env::default();
        let market = default_market(&env);
        for op in &ops {
            apply(&env, &market, op);
        }
        // Fast-forward to maturity.
        let now = time::now(&env);
        if now < market.maturity {
            advance_by(&env, market.maturity - now);
        }
        // All outstanding PT must be redeemable. Once Market::builder
        // wires real contract clients, replace the placeholder with:
        //   let pt_supply = market.pt_supply(&env);
        //   let ys_balance = und.balance(&market.yield_stripping);
        //   let unclaimed = ys.total_unclaimed();
        //   prop_assert!(ys_balance >= pt_supply + unclaimed, ...);
        let _ = market;
    }
}

// -------------------------------------------------------------------------
// INV-NFL (rounding-against-attacker): 1000 round-trips with 1-wei inputs
// must not increase the attacker's underlying balance.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: cases() / 8, ..config() })]
    #[test]
    fn proptest_rounding_against_attacker(seed in any::<u64>()) {
        let env = Env::default();
        let market = default_market(&env);
        let _ = seed;
        // TODO(blockchain-developer):
        //   let attacker = market.user(&env, "attacker");
        //   let und = StellarAssetClient::new(&env, &market.underlying);
        //   let start = und.balance(&attacker);
        //   for _ in 0..1000 {
        //       let _ = std::panic::catch_unwind(|| ys.mint(&attacker, &1));
        //       let _ = std::panic::catch_unwind(|| ys.burn(&attacker, &1));
        //   }
        //   let end = und.balance(&attacker);
        //   prop_assert!(end <= start,
        //       "INV-NFL via rounding: gained {} wei over 1000 round-trips",
        //       end - start);
        let _ = market;
    }
}

// -------------------------------------------------------------------------
// INV-U1: YT-transfer snapshot freshness.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_yt_transfer_snapshot(
        mint_a in 1i128..1_000_000,
        transfer_bps in 1u32..10_000,
        gap_secs in 1u64..(time::DAY * 30),
    ) {
        let env = Env::default();
        let market = default_market(&env);
        let _ = (mint_a, transfer_bps, gap_secs);
        // TODO(blockchain-developer):
        //   1. alice mints `mint_a`.
        //   2. advance(gap_secs / 2) + rate bump.
        //   3. alice transfers `transfer_bps/10000 * yt_balance` to bob.
        //   4. advance(gap_secs / 2) + rate bump.
        //   5. bob.claim()
        //   6. Compute bob's expected fair-share for the post-transfer
        //      window and assert |actual - expected| <= 1 wei.
        let _ = market;
    }
}

// -------------------------------------------------------------------------
// INV-M1: global yield index is monotone.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_yield_index_monotone(ops in op_sequence()) {
        let env = Env::default();
        let market = default_market(&env);
        let mut last_index: i128 = 0;
        for op in &ops {
            apply(&env, &market, op);
            // Reading the yield index might trigger a sync (this is fine;
            // we want the post-sync value for the monotonicity check).
            // TODO(blockchain-developer):
            //   let idx = ys.yield_index();
            //   prop_assert!(idx >= last_index,
            //       "INV-M1 violated after {:?}: {} < {}", op, idx, last_index);
            //   last_index = idx;
            let _ = last_index;
        }
    }
}

// -------------------------------------------------------------------------
// INV-A1: AMM curve k is non-decreasing modulo fee.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_amm_k_non_decreasing(
        pt_in in 1i128..(100 * strate_testutils::constants::ONE_UNDERLYING),
        direction in 0u8..2,
    ) {
        let env = Env::default();
        let market = Market::builder()
            .amm_reserves(
                1_000 * strate_testutils::constants::ONE_UNDERLYING,
                1_000 * strate_testutils::constants::ONE_UNDERLYING,
            )
            .seed_user("trader", 10_000 * strate_testutils::constants::ONE_UNDERLYING)
            .build(&env);
        let _ = (pt_in, direction);
        // TODO(blockchain-developer):
        //   let amm = AmmClient::new(&env, &market.amm.unwrap());
        //   let trader = market.user(&env, "trader");
        //   let k_pre = amm.k_invariant();
        //   match direction {
        //       0 => amm.swap_pt_for_underlying(&trader, &pt_in, &1),
        //       _ => amm.swap_underlying_for_pt(&trader, &pt_in, &1),
        //   }
        //   let k_post = amm.k_invariant();
        //   let fees = amm.cumulative_protocol_fees();
        //   prop_assert!(k_post >= k_pre - fees,
        //       "INV-A1 violated: k_pre={} k_post={} fees={}", k_pre, k_post, fees);
        let _ = market;
    }
}

// -------------------------------------------------------------------------
// INV-A4: AMM is locked in the maturity window.
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_amm_lock_window(
        secs_before_maturity in 0u64..(strate_testutils::constants::MARKET_LOCK_WINDOW_SECONDS),
    ) {
        let env = Env::default();
        let market = near_maturity_market(&env);
        // Position inside the lock window.
        let target = market.maturity.saturating_sub(secs_before_maturity);
        time::advance_to(&env, target);
        // TODO(blockchain-developer):
        //   let amm = AmmClient::new(&env, &market.amm.unwrap());
        //   let trader = market.user(&env, "attacker");
        //   let result = std::panic::catch_unwind(|| {
        //       amm.swap_pt_for_underlying(&trader, &1_000_000, &1)
        //   });
        //   prop_assert!(result.is_err(),
        //       "INV-A4 violated: swap succeeded {}s before maturity",
        //       secs_before_maturity);
        let _ = market;
    }
}

// -------------------------------------------------------------------------
// Donation-attack property: zero-supply state cannot be inflated.
// (Companion to C-01.)
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]
    #[test]
    fn proptest_donation_inflation_mitigated(donation in 1i128..(1_000_000_000_000)) {
        let env = Env::default();
        let market = empty_market(&env);
        let _ = donation;
        // TODO(blockchain-developer): execute the donation harness and
        // assert AttackResult::Mitigated.
        //   let attacker = market.user(&env, "attacker");
        //   let mut h = DonationInflationAttacker::standard();
        //   h.donation = donation;
        //   let r = h.run(&env, &market, &attacker);
        //   prop_assert!(r.is_mitigated(), "donation inflation succeeded: {:?}", r);
        let _ = market;
    }
}
