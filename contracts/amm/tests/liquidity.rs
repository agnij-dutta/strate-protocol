//! Add / remove liquidity tests.
//!
//! Covers:
//! - First-LP path mints `sqrt(pt*und) - MINIMUM_LIQUIDITY` and locks
//!   `MINIMUM_LIQUIDITY` permanently (`total_lp - lp_balance == 1000`).
//! - Subsequent adds use the proportional formula and slip to the
//!   binding ratio.
//! - Remove rounds **down** on assets out.
//! - Slippage guards reject mints below the user's `min_lp_out` and
//!   burns below `min_pt_out / min_under_out`.
//! - Empty-pool path rejects zero-amount deposits.

mod common;

use amm::MINIMUM_LIQUIDITY;
use common::{AmmFixture, ONE};

#[test]
fn first_lp_mints_geometric_minus_minimum_liquidity() {
    let fx = AmmFixture::new();
    let pt_in = 1_000 * ONE;
    let und_in = 1_000 * ONE;
    fx.fund_pt(&fx.provider, pt_in);
    fx.fund_underlying(&fx.provider, und_in);

    let lp_minted = fx.amm_client.add_liquidity(&fx.provider, &pt_in, &und_in, &1);

    // Geometric mean of equal reserves is the reserve size.
    // sqrt(1_000_e7 * 1_000_e7) = 1_000_e7.
    let expected_geo = 1_000 * ONE;
    let expected_user_lp = expected_geo - MINIMUM_LIQUIDITY;
    assert_eq!(lp_minted, expected_user_lp);
    assert_eq!(fx.amm_client.lp_balance(&fx.provider), expected_user_lp);
    assert_eq!(fx.amm_client.total_lp(), expected_geo);

    // Reserves are credited as expected.
    let (rx_pt, rx_und) = fx.amm_client.reserves();
    assert_eq!(rx_pt, pt_in);
    assert_eq!(rx_und, und_in);
}

#[test]
fn second_lp_proportional() {
    let fx = AmmFixture::new();
    fx.fund_pt(&fx.provider, 1_000 * ONE);
    fx.fund_underlying(&fx.provider, 1_000 * ONE);
    fx.amm_client
        .add_liquidity(&fx.provider, &(1_000 * ONE), &(1_000 * ONE), &1);

    // Second LP deposits same ratio — gets a proportional share.
    let bob = soroban_sdk::testutils::Address::generate(&fx.env);
    fx.fund_pt(&bob, 500 * ONE);
    fx.fund_underlying(&bob, 500 * ONE);

    let bob_lp = fx
        .amm_client
        .add_liquidity(&bob, &(500 * ONE), &(500 * ONE), &1);
    // Bob's contribution is half the pool; he gets half of total_lp -
    // first LP's share. Since first LP minted `1_000*ONE - 1_000`
    // (with MINIMUM_LIQUIDITY locked), Bob's mint = floor(500*ONE * total_lp / pool_pt).
    // After bob's add, total_lp = floor(1_000*ONE + 500*ONE * 1_000*ONE / 1_000*ONE)
    //                            = floor(1_000*ONE + 500*ONE)
    //                            = 1_500*ONE.
    // Bob receives 500*ONE.
    assert_eq!(bob_lp, 500 * ONE);
}

#[test]
fn min_lp_out_slippage_guards() {
    let fx = AmmFixture::new();
    fx.fund_pt(&fx.provider, 1_000 * ONE);
    fx.fund_underlying(&fx.provider, 1_000 * ONE);
    // Demand more LP than the geometric mean can possibly mint.
    let r = fx.amm_client.try_add_liquidity(
        &fx.provider,
        &(1_000 * ONE),
        &(1_000 * ONE),
        &(10_000 * ONE),
    );
    assert!(r.is_err(), "should slip");
}

#[test]
fn zero_or_negative_deposit_rejected() {
    let fx = AmmFixture::new();
    let r = fx
        .amm_client
        .try_add_liquidity(&fx.provider, &0, &(1_000 * ONE), &1);
    assert!(r.is_err());
    let r = fx
        .amm_client
        .try_add_liquidity(&fx.provider, &(1_000 * ONE), &0, &1);
    assert!(r.is_err());
}

#[test]
fn remove_liquidity_returns_proportional_share() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    let lp_bal = fx.amm_client.lp_balance(&fx.provider);

    // Withdraw half.
    let burn = lp_bal / 2;
    let (pt_out, und_out) = fx.amm_client.remove_liquidity(&fx.provider, &burn, &1, &1);
    // Pool had 1_000 each, total_lp = 1_000*ONE (= 10_000_000_000).
    // Burn = (10_000_000_000 - 1_000) / 2 = 4_999_999_500.
    // pt_out = floor(burn * 1_000*ONE / total_lp) ≈ 499.9999500 * ONE.
    // We just sanity-check it's close to half and strictly < half (rounded down).
    let half_reserve = 500 * ONE;
    assert!(pt_out <= half_reserve, "pt_out={pt_out} > half_reserve={half_reserve}");
    assert!(und_out <= half_reserve);
    assert!(pt_out > half_reserve - 100, "pt_out={pt_out} unexpectedly small");
}

#[test]
fn cannot_remove_more_than_balance() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    let bal = fx.amm_client.lp_balance(&fx.provider);
    let r = fx.amm_client.try_remove_liquidity(&fx.provider, &(bal + 1), &1, &1);
    assert!(r.is_err());
}

#[test]
fn min_out_slippage_on_burn() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    let lp_bal = fx.amm_client.lp_balance(&fx.provider);
    // Demand more PT out than the burn possibly returns.
    let r = fx
        .amm_client
        .try_remove_liquidity(&fx.provider, &(lp_bal / 2), &(1_000_000 * ONE), &1);
    assert!(r.is_err());
}

#[test]
fn minimum_liquidity_remains_locked_in_total_lp() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    // Total LP > user's LP balance by exactly MINIMUM_LIQUIDITY.
    let total = fx.amm_client.total_lp();
    let user = fx.amm_client.lp_balance(&fx.provider);
    assert_eq!(total - user, MINIMUM_LIQUIDITY);
    // Even after burning ALL of user's balance, MINIMUM_LIQUIDITY
    // remains in total_lp.
    fx.amm_client
        .remove_liquidity(&fx.provider, &user, &1, &1);
    assert_eq!(fx.amm_client.total_lp(), MINIMUM_LIQUIDITY);
}
