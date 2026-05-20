//! C-03 — Near-maturity AMM drain mitigation.
//!
//! Asserts that any swap attempt within `MARKET_LOCK_WINDOW_SECONDS` of
//! maturity reverts with `MarketLockedNearMaturity`, and that LP burn
//! remains available throughout the lock window.

mod common;

use amm::MARKET_LOCK_WINDOW_SECONDS;
use common::{AmmFixture, ONE};

#[test]
fn swap_reverts_inside_lock_window() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);

    // Move to one second inside the lock window.
    let to_advance = fx.maturity - common::START_TS - MARKET_LOCK_WINDOW_SECONDS + 1;
    fx.advance(to_advance);

    fx.fund_pt(&fx.trader, 10 * ONE);
    let r = fx
        .amm_client
        .try_swap_exact_pt_for_sy(&fx.trader, &(10 * ONE), &1);
    assert!(r.is_err(), "swap inside lock window must revert");
}

#[test]
fn swap_reverts_one_second_before_maturity() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    // Position 1 second before maturity — deep in the asymptote region.
    let to_advance = fx.maturity - common::START_TS - 1;
    fx.advance(to_advance);

    fx.fund_pt(&fx.trader, 10 * ONE);
    let r = fx
        .amm_client
        .try_swap_exact_pt_for_sy(&fx.trader, &(10 * ONE), &1);
    assert!(r.is_err(), "swap 1s before maturity must revert");
}

#[test]
fn swap_succeeds_just_outside_lock_window() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    // Advance to exactly one second outside the lock window — the
    // entrypoint requires `t_remaining > MARKET_LOCK_WINDOW_SECONDS`
    // strictly, so `t_remaining == MARKET_LOCK_WINDOW_SECONDS + 1` is
    // the first ledger that admits a swap.
    let to_advance = fx.maturity - common::START_TS - MARKET_LOCK_WINDOW_SECONDS - 1;
    fx.advance(to_advance);

    fx.fund_pt(&fx.trader, 10 * ONE);
    let out = fx
        .amm_client
        .swap_exact_pt_for_sy(&fx.trader, &(10 * ONE), &1);
    assert!(out > 0);
}

#[test]
fn lp_burn_allowed_during_lock_window() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    // Enter the lock window.
    let to_advance = fx.maturity - common::START_TS - MARKET_LOCK_WINDOW_SECONDS + 60;
    fx.advance(to_advance);

    let lp_bal = fx.amm_client.lp_balance(&fx.provider);
    // Burn half — LP path is unrestricted by the lock since it's pure
    // proportional withdrawal.
    let (pt_out, und_out) = fx
        .amm_client
        .remove_liquidity(&fx.provider, &(lp_bal / 2), &1, &1);
    assert!(pt_out > 0 && und_out > 0);
}

#[test]
fn swap_reverts_post_maturity() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);
    // Cross maturity entirely.
    let to_advance = fx.maturity - common::START_TS + 1;
    fx.advance(to_advance);

    fx.fund_pt(&fx.trader, 10 * ONE);
    let r = fx
        .amm_client
        .try_swap_exact_pt_for_sy(&fx.trader, &(10 * ONE), &1);
    assert!(r.is_err());
}

/// Mirrors the `NearMaturityDrainAttacker` harness contract in
/// `strate-testutils` — but composed inline because the harness's
/// `Market` builder is still stubbed pending the parallel agent's
/// integration. Once the builder lands, this test should switch to
/// invoking `NearMaturityDrainAttacker::run` directly.
#[test]
fn near_maturity_drain_attacker_is_mitigated() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000 * ONE, 1_000 * ONE);

    // Position 1 second before maturity.
    let to_advance = fx.maturity - common::START_TS - 1;
    fx.advance(to_advance);

    // Attacker funds and attempts to extract a large output by
    // accepting any min_out.
    fx.fund_pt(&fx.trader, 10_000 * ONE);
    let r = fx
        .amm_client
        .try_swap_exact_pt_for_sy(&fx.trader, &(10 * ONE), &1);
    // Mitigation expectation: the swap reverts. Any non-error here
    // would imply the attacker successfully drained underlying using
    // the asymptote.
    assert!(r.is_err(), "C-03: near-maturity swap must revert");
}
