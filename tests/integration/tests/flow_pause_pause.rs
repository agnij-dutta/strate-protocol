//! Pause propagation:
//!
//! - YS.pause blocks `mint` but NOT `claim_yield` or `redeem_*`
//!   (ADR-002 §"While paused"). User exits remain open.
//! - AMM.pause blocks all liquidity operations and swaps.
//! - Oracle.pause blocks all reads (downstream effect: YS.sync fails).

use strate_integration_tests::Fixture;
use strate_testutils::constants::ONE_UNDERLYING;

#[test]
fn ys_pause_blocks_mint_but_not_claim_or_redeem() {
    let f = Fixture::default_market();
    let alice = f.user("alice");
    f.fund(&alice, 2_000 * ONE_UNDERLYING);

    // Mint pre-pause.
    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));
    f.advance_days(10);

    // Pause.
    f.ys().pause();
    assert!(f.ys().is_paused());

    // Mint must revert.
    let r = f.ys().try_mint(&alice, &alice, &(100 * ONE_UNDERLYING));
    assert!(
        matches!(r, Ok(Err(_))) || r.is_err(),
        "paused mint should revert"
    );

    // Redeem-pair must still succeed.
    let r = f.ys().try_redeem_pair(&alice, &(100 * ONE_UNDERLYING));
    assert!(r.is_ok(), "redeem_pair should succeed while paused");

    // Claim must still succeed (returns 0 if no accrual).
    let r = f.ys().try_claim_yield(&alice);
    assert!(r.is_ok(), "claim_yield should succeed while paused");

    f.assert_invariants();
}

#[test]
fn ys_unpause_is_timelocked() {
    let f = Fixture::default_market();
    f.ys().pause();
    assert!(f.ys().is_paused());

    // Open the unpause proposal.
    f.ys().propose_unpause();
    let eta = f.ys().unpause_eta();
    assert!(eta > 0);

    // Execute before eta: must fail.
    let r = f.ys().try_execute_unpause();
    assert!(matches!(r, Ok(Err(_))) || r.is_err());

    // Advance past the timelock.
    strate_testutils::time::advance_by(&f.env, 48 * 60 * 60 + 1);

    f.ys().execute_unpause();
    assert!(!f.ys().is_paused());
}

#[test]
fn amm_pause_blocks_swap_and_liquidity() {
    let f = Fixture::default_market();
    let alice = f.user("alice");

    f.fund(&alice, 2_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.amm()
        .add_liquidity(&alice, &(500 * ONE_UNDERLYING), &(500 * ONE_UNDERLYING), &1);

    f.amm().pause();

    let r = f
        .amm()
        .try_swap_exact_pt_for_sy(&alice, &(10 * ONE_UNDERLYING), &1);
    assert!(matches!(r, Ok(Err(_))) || r.is_err());

    let r = f
        .amm()
        .try_add_liquidity(&alice, &(10 * ONE_UNDERLYING), &(10 * ONE_UNDERLYING), &1);
    assert!(matches!(r, Ok(Err(_))) || r.is_err());

    f.amm().unpause();
    // Now things work again.
    f.amm()
        .add_liquidity(&alice, &(10 * ONE_UNDERLYING), &(10 * ONE_UNDERLYING), &1);
}

#[test]
fn oracle_pause_blocks_sync_and_reads() {
    let f = Fixture::default_market();
    let _ = f.oracle().sync_rate();
    f.oracle().pause();
    assert!(f.oracle().is_paused());

    let r = f.oracle().try_sync_rate();
    assert!(matches!(r, Ok(Err(_))) || r.is_err());

    let r = f.oracle().try_get_rate();
    assert!(matches!(r, Ok(Err(_))) || r.is_err());
}
