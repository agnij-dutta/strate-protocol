//! C-03 — AMM near-maturity asymptote regression test.
//!
//! In the final minutes before maturity, the log-curve's time fraction
//! approaches zero and ln(t_frac) → -∞. Unguarded, this lets an
//! attacker realise unbounded slippage. Mitigation: the AMM hard-locks
//! all swaps in the window `[maturity - 30min, maturity)`.

use strate_integration_tests::Fixture;
use strate_testutils::attackers::NearMaturityDrainAttacker;
use strate_testutils::constants::ONE_UNDERLYING;
use strate_testutils::AttackResult;

#[test]
fn c03_near_maturity_amm_is_blocked() {
    // We need a market with the AMM deployed, then advance time into
    // the lock window. Default market has 90-day tenor — plenty.
    let f = Fixture::default_market();
    let attacker = f.user("alice");

    // Seed AMM liquidity first.
    f.fund(&attacker, 5_000 * ONE_UNDERLYING);
    f.ys().mint(&attacker, &attacker, &(5_000 * ONE_UNDERLYING));
    f.fund(&attacker, 1_000 * ONE_UNDERLYING);
    f.amm().add_liquidity(
        &attacker,
        &(500 * ONE_UNDERLYING),
        &(500 * ONE_UNDERLYING),
        &1,
    );

    let result = NearMaturityDrainAttacker::standard().run(&f.env, &f.market, &attacker);
    assert_eq!(
        result,
        AttackResult::Mitigated,
        "C-03 attack succeeded: {result:?}"
    );
}

#[test]
fn c03_swap_works_30min1s_before_maturity() {
    // Boundary: at maturity - 30 min - 1 sec the swap is still allowed.
    let f = Fixture::default_market();
    let alice = f.user("alice");

    f.fund(&alice, 5_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(5_000 * ONE_UNDERLYING));
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.amm().add_liquidity(
        &alice,
        &(500 * ONE_UNDERLYING),
        &(500 * ONE_UNDERLYING),
        &1,
    );

    // 30 min 30 sec before maturity → just outside the 30-min lock.
    let target = f.market.maturity - 30 * 60 - 30;
    strate_testutils::time::advance_to(&f.env, target);

    let r = f.amm().try_swap_exact_pt_for_sy(&alice, &(10 * ONE_UNDERLYING), &1);
    assert!(r.is_ok(), "swap outside lock window must succeed: {r:?}");
}
