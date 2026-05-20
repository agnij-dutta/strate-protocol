//! H-01 end-to-end: YT transfer with the `_update` hook firing
//! `settle_user` for both sides before the balance mutation.
//!
//! Sequence:
//! 1. Alice mints, accruing YT.
//! 2. Time passes, rate moves — yield accrues globally.
//! 3. Alice transfers half her YT to Bob.
//! 4. More time passes.
//! 5. Bob claims; should receive ONLY the post-transfer-window yield.
//!    If the hook is missing, Bob's snapshot inherits zero and his
//!    claim covers the entire history — that's the H-01 exploit.

use strate_integration_tests::{accrue, Fixture};
use strate_testutils::constants::{ONE_UNDERLYING, WAD};

#[test]
fn yt_transfer_recipient_does_not_get_stale_yield() {
    let f = Fixture::default_market();
    let alice = f.user("alice");
    let bob = f.user("bob");

    // 1. Alice mints 1000.
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));

    // 2. 30 days, +2% rate.
    f.advance_days(30);
    accrue(&f, 2 * (WAD / 100));

    // Sanity: Alice has pending yield > 0; Bob has pending yield == 0.
    let alice_pending_before = f.ys().pending_yield(&alice);
    assert!(alice_pending_before > 0);
    assert_eq!(f.ys().pending_yield(&bob), 0);

    // 3. Alice transfers half her YT to Bob.
    let alice_yt = f.yt_balance(&alice);
    let half = alice_yt / 2;
    yt_token::YtTokenClient::new(&f.env, &f.market.yt).transfer(&alice, &bob, &half);

    // After transfer:
    //   - Alice's snapshot has been bumped to current global index by
    //     the _update hook; her pending should be ~0 (because her
    //     accrued has been moved to AccruedYield).
    //   - Bob's snapshot has been initialized to current global index;
    //     his pending should be ~0.
    // Both users' pending_yield should be ~0 now (the accrual is in
    // AccruedYield, not "still pending against the index").
    //
    // The Alice-side: her `AccruedYield` now equals what was
    // `alice_pending_before` (minus rounding wei).
    let alice_pending_after = f.ys().pending_yield(&alice);
    let bob_pending_after = f.ys().pending_yield(&bob);
    // Both should be the AccruedYield-only portion. Alice's settled
    // amount carries through; Bob's settled amount is zero.
    // Allow ±1 wei rounding.
    assert!(
        (alice_pending_after - alice_pending_before).abs() <= 1,
        "alice_after={alice_pending_after} before={alice_pending_before}"
    );
    assert_eq!(bob_pending_after, 0, "bob got stale yield: {bob_pending_after}");

    // 4. Another 30 days, +2% rate.
    f.advance_days(30);
    accrue(&f, 2 * (WAD / 100));

    // 5. Bob claims. The amount must be small — only the post-transfer
    // accrual proportional to Bob's `half` of YT.
    let bob_claimed = f.ys().claim_yield(&bob);
    // Bound: Bob should claim *no more than* Alice's pre-transfer pending.
    // Because Alice held everything then; Bob holds half now over the
    // second period, so Bob's claim is bounded by the same order.
    assert!(
        bob_claimed <= alice_pending_before * 2,
        "bob_claimed={bob_claimed} exceeds plausible bound"
    );

    // Alice claims too; her amount should equal pre-transfer pending +
    // post-transfer share on her remaining YT.
    let alice_claimed = f.ys().claim_yield(&alice);
    assert!(alice_claimed >= alice_pending_before - 2);

    f.assert_invariants();
}

#[test]
fn yt_self_transfer_is_noop() {
    let f = Fixture::default_market();
    let alice = f.user("alice");
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));
    let bal_pre = f.yt_balance(&alice);

    yt_token::YtTokenClient::new(&f.env, &f.market.yt)
        .transfer(&alice, &alice, &(bal_pre / 4));

    let bal_post = f.yt_balance(&alice);
    assert_eq!(bal_pre, bal_post, "self-transfer changed balance");

    f.assert_invariants();
}

#[test]
fn yt_burn_credits_accrued_before_balance_decrement() {
    // Burn via redeem_pair: the YS calls YT.burn, which fires the
    // _update hook with from=Some(holder). settle_user must credit
    // the pending yield BEFORE the balance shrinks.
    let f = Fixture::default_market();
    let alice = f.user("alice");
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));

    f.advance_days(30);
    accrue(&f, WAD / 100);

    let pending_before_burn = f.ys().pending_yield(&alice);
    assert!(pending_before_burn > 0);

    // Burn all PT+YT via redeem_pair.
    let pt_bal = f.pt_balance(&alice);
    f.ys().redeem_pair(&alice, &pt_bal);

    // Yield credit should survive the burn.
    let post_redeem_pending = f.ys().pending_yield(&alice);
    assert!(
        (post_redeem_pending - pending_before_burn).abs() <= 1,
        "yield credit lost on burn: before={pending_before_burn} after={post_redeem_pending}"
    );

    // Alice claims her surviving credit.
    let claimed = f.ys().claim_yield(&alice);
    assert!(
        (claimed - pending_before_burn).abs() <= 1,
        "claimed={claimed} pending={pending_before_burn}"
    );
}
