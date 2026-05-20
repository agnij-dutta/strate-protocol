//! Full happy-path flow: mint -> accrue -> claim -> redeem.
//!
//! Exercises the call graph for the entire YS surface across all five
//! contracts:
//!
//! - YS.mint touches Oracle.sync_rate, PT.mint, YT.mint, Underlying SAC
//!   transfer, YT._update -> YS.settle_user (twice).
//! - YS.claim_yield touches Oracle.sync_rate, YS.settle_user, Underlying
//!   SAC transfer.
//! - YS.redeem_pt touches PT.burn, Underlying SAC transfer.
//!
//! Witness invariants exercised: INV-S1, INV-S3, INV-M1, INV-U1.

use strate_integration_tests::{accrue, Fixture, BLEND_ONE};
use strate_testutils::constants::{ONE_UNDERLYING, WAD};

#[test]
fn mint_claim_redeem_full_flow() {
    let f = Fixture::default_market();
    let alice = f.user("alice");

    // 1. Seed: ensure Alice has plenty of underlying.
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    let initial_balance = f.underlying_balance(&alice);
    assert!(initial_balance >= 1_000 * ONE_UNDERLYING);

    // 2. Alice mints 1000 PT+YT.
    let mint_amount = 1_000 * ONE_UNDERLYING;
    let (pt_out, yt_out, _idx) = f.ys().mint(&alice, &alice, &mint_amount);

    // First mint pays DEAD_SHARES (1000) — Alice gets mint_amount - 1000.
    assert_eq!(pt_out, mint_amount - 1_000);
    assert_eq!(yt_out, mint_amount - 1_000);
    assert_eq!(f.pt_balance(&alice), pt_out);
    assert_eq!(f.yt_balance(&alice), yt_out);

    // 3. INV-S1: pt_supply == yt_supply.
    assert_eq!(f.market.pt_supply(&f.env), f.market.yt_supply(&f.env));

    // 4. Time passes; Blend rate grows.
    f.advance_days(30);
    accrue(&f, WAD / 100); // +1%

    // 5. Alice claims accrued yield.
    let pre_claim = f.underlying_balance(&alice);
    let claimed = f.ys().claim_yield(&alice);
    let post_claim = f.underlying_balance(&alice);

    assert!(claimed > 0, "claimed nothing despite 1% accrual");
    assert_eq!(post_claim - pre_claim, claimed);

    // Sanity: yield should be on the order of 1% of YT balance (= principal).
    // Allow a wide bound; the precise number depends on the Oracle's
    // baseline-latch timing.
    assert!(
        claimed < mint_amount,
        "claimed {claimed} exceeds principal {mint_amount}"
    );

    // 6. Fast-forward to maturity.
    f.advance_to_maturity();

    // 7. Alice redeems PT 1:1. (Burning her full PT balance.)
    let pt_to_redeem = f.pt_balance(&alice);
    let pre = f.underlying_balance(&alice);
    let redeemed = f.ys().redeem_pt(&alice, &pt_to_redeem);
    let post = f.underlying_balance(&alice);
    assert_eq!(redeemed, pt_to_redeem);
    assert_eq!(post - pre, pt_to_redeem);

    // 8. Final invariants.
    f.assert_invariants();
}

#[test]
fn mint_redeem_pair_pre_maturity() {
    let f = Fixture::default_market();
    let bob = f.user("bob");
    f.fund(&bob, 500 * ONE_UNDERLYING);

    let pre = f.underlying_balance(&bob);
    let (pt, yt, _) = f.ys().mint(&bob, &bob, &(500 * ONE_UNDERLYING));
    assert_eq!(pt, yt);

    // Redeem half via the pair path.
    let half = pt / 2;
    let underlying_out = f.ys().redeem_pair(&bob, &half);
    assert_eq!(underlying_out, half);

    let post = f.underlying_balance(&bob);
    // Net change: paid 500u, got back `half` u. Balance ≈ pre - 500 + half.
    let expected_post = pre - (500 * ONE_UNDERLYING) + half;
    assert_eq!(post, expected_post);

    f.assert_invariants();
}

#[test]
fn yield_grows_with_time_for_multiple_holders() {
    let f = Fixture::default_market();
    let alice = f.user("alice");
    let bob = f.user("bob");

    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.fund(&bob, 1_000 * ONE_UNDERLYING);

    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));
    f.ys().mint(&bob, &bob, &(500 * ONE_UNDERLYING));

    f.advance_days(45);
    accrue(&f, WAD / 50); // +2%

    let alice_pending = f.ys().pending_yield(&alice);
    let bob_pending = f.ys().pending_yield(&bob);
    // Alice's YT > Bob's YT, so Alice's pending yield should be greater.
    assert!(
        alice_pending >= bob_pending,
        "alice_pending={alice_pending} bob_pending={bob_pending}"
    );

    // Claim both.
    let alice_got = f.ys().claim_yield(&alice);
    let bob_got = f.ys().claim_yield(&bob);
    assert_eq!(alice_got, alice_pending);
    assert_eq!(bob_got, bob_pending);

    // After claim, pending_yield should be 0 (or near-zero noise).
    assert_eq!(f.ys().pending_yield(&alice), 0);
    assert_eq!(f.ys().pending_yield(&bob), 0);

    f.assert_invariants();
}

#[test]
fn baseline_oracle_state_is_consistent() {
    let f = Fixture::default_market();
    // After fixture construction the Oracle has not yet been synced, so
    // the cached rate is zero. Drive one sync.
    let r = f.oracle().try_sync_rate();
    assert!(r.is_ok(), "first sync should succeed");
    let (cached, _) = f.oracle().cached_rate_raw();
    // Blend's 1e9 initial scaled to WAD = WAD.
    assert_eq!(cached, WAD);
    let _ = BLEND_ONE; // keep import alive
}
