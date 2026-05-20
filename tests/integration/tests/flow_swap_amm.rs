//! AMM swap flow + interaction with YS.
//!
//! Exercises: YS.mint to create PT, then AMM.swap_exact_pt_for_sy
//! transfers PT into the AMM pool, returns underlying out. Verifies:
//!
//! - AMM does not touch YS state (per ADR-001 §"Forbidden edges").
//! - PT supply invariant holds — swap is a transfer, not a mint/burn.
//! - Slippage guard works.

use strate_integration_tests::Fixture;
use strate_testutils::constants::ONE_UNDERLYING;

#[test]
fn swap_pt_for_underlying_basic() {
    let f = Fixture::default_market();
    let alice = f.user("alice");

    // Mint PT+YT via YS.
    f.fund(&alice, 2_000 * ONE_UNDERLYING);
    let (pt_out, _yt_out, _) = f.ys().mint(&alice, &alice, &(2_000 * ONE_UNDERLYING));

    // The AMM expects PT and underlying; we have PT from the mint and
    // some leftover underlying. Seed the pool first.
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    let lp = f.amm().add_liquidity(
        &alice,
        &(500 * ONE_UNDERLYING),
        &(500 * ONE_UNDERLYING),
        &1,
    );
    assert!(lp > 0);

    // Snapshot pt_supply before swap (should be unchanged by swap).
    let pt_supply_pre = f.market.pt_supply(&f.env);

    // Now swap 100 PT for underlying. The AMM transfers PT in and
    // underlying out.
    let pt_in = 100 * ONE_UNDERLYING;
    let pre_under = f.underlying_balance(&alice);
    let out = f
        .amm()
        .swap_exact_pt_for_sy(&alice, &pt_in, &1);
    assert!(out > 0, "swap returned zero output");
    let post_under = f.underlying_balance(&alice);
    assert_eq!(post_under - pre_under, out);

    // PT supply unchanged — swap moves PT between addresses but does
    // not burn it.
    assert_eq!(pt_supply_pre, f.market.pt_supply(&f.env));

    // Invariants still hold.
    f.assert_invariants();
}

#[test]
fn swap_slippage_guard_reverts() {
    let f = Fixture::default_market();
    let alice = f.user("alice");

    f.fund(&alice, 2_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(2_000 * ONE_UNDERLYING));
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.amm().add_liquidity(
        &alice,
        &(500 * ONE_UNDERLYING),
        &(500 * ONE_UNDERLYING),
        &1,
    );

    // Quote a swap first to find a realistic min_out, then ask for 10x
    // that as min_out — should revert with SlippageExceeded.
    let pt_in = 100 * ONE_UNDERLYING;
    let absurd_min = pt_in * 10;
    let r = f.amm().try_swap_exact_pt_for_sy(&alice, &pt_in, &absurd_min);
    assert!(r.is_err() || matches!(r, Ok(Err(_))), "swap must revert");
}

#[test]
fn amm_lock_window_blocks_swap() {
    // Build a market just over the AMM's minimum (1 day + 30 min lock
    // window + 60s headroom), then advance to inside the lock window.
    let env = soroban_sdk::Env::default();
    let market = strate_testutils::builders::MarketBuilder::default()
        .tenor(86_400 + 1_800 + 600) // 1 day + 30m + 10m
        .seed_user("alice", 2_000 * ONE_UNDERLYING)
        .build(&env);
    let f = strate_integration_tests::Fixture { env, market };

    let alice = f.user("alice");
    f.fund(&alice, 2_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(2_000 * ONE_UNDERLYING));
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.amm().add_liquidity(
        &alice,
        &(500 * ONE_UNDERLYING),
        &(500 * ONE_UNDERLYING),
        &1,
    );

    // Position 15 minutes before maturity — inside the 30-min lock.
    strate_testutils::time::advance_to(&f.env, f.market.maturity - 15 * 60);

    let r = f.amm().try_swap_exact_pt_for_sy(
        &alice,
        &(10 * ONE_UNDERLYING),
        &1,
    );
    assert!(
        r.is_err() || matches!(r, Ok(Err(_))),
        "lock window must block swap"
    );
}

#[test]
fn amm_remove_liquidity_works_inside_lock_window() {
    // Confirms that LP exit is NOT blocked by the lock window — only
    // swaps are. INV-L2 corollary.
    let env = soroban_sdk::Env::default();
    let market = strate_testutils::builders::MarketBuilder::default()
        .tenor(86_400 + 1_800 + 600)
        .seed_user("alice", 2_000 * ONE_UNDERLYING)
        .build(&env);
    let f = strate_integration_tests::Fixture { env, market };

    let alice = f.user("alice");
    f.fund(&alice, 2_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(2_000 * ONE_UNDERLYING));
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    let lp = f.amm().add_liquidity(
        &alice,
        &(500 * ONE_UNDERLYING),
        &(500 * ONE_UNDERLYING),
        &1,
    );
    strate_testutils::time::advance_to(&f.env, f.market.maturity - 15 * 60);

    let (pt_out, under_out) = f.amm().remove_liquidity(&alice, &(lp / 2), &1, &1);
    assert!(pt_out > 0 && under_out > 0);
}
