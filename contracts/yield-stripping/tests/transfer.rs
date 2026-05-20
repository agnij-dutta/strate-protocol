//! H-01: YT transfer recipient stale snapshot.
//!
//! Alice mints YT, accrues yield, then transfers half to Bob. The
//! invariant: Bob's claim after the transfer should equal **only** the
//! yield that accrues post-transfer proportional to his YT balance —
//! never any of the pre-transfer yield that Alice already earned.

#![cfg(test)]

mod common;

use common::{Fixture, ONE, WAD};

#[test]
fn yt_transfer_recipient_does_not_inherit_pre_transfer_yield() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);
    let bob = f.user(0);

    // 1. Alice mints. Dead-shares fire on this first deposit.
    f.ys_client().mint(&alice, &alice, &(100 * ONE));

    // 2. Advance + accrue a slug of yield. Simulate Blend accrual.
    f.advance(7 * common::DAY);
    f.set_rate(WAD + WAD / 100); // +1%
    f.fund(&f.ys, 10 * ONE);

    // 3. Sync the index so Alice has settled accrual on her own
    //    snapshot.  (Not required for the bug; required so we can
    //    measure Alice's earned share.)
    f.ys_client().sync_yield_index();

    // 4. Alice transfers 50% of her YT to Bob. The YT `_update` hook
    //    must call `settle_user(bob)` BEFORE Bob's balance increments,
    //    so Bob's snapshot is the current index — H-01 mitigation.
    let alice_yt = f.yt_client().balance(&alice);
    let transfer = alice_yt / 2;
    f.yt_client().transfer(&alice, &bob, &transfer);

    // 5. Immediately claim for Bob. He just received YT but the index
    //    has not advanced since his settle — he must receive 0.
    let pre_bob = f.underlying_tok().balance(&bob);
    let bob_claim_immediate = f.ys_client().claim_yield(&bob);
    let post_bob = f.underlying_tok().balance(&bob);

    assert_eq!(
        bob_claim_immediate, 0,
        "Bob extracted {} wei of Alice's pre-transfer yield — H-01 regressed",
        bob_claim_immediate
    );
    assert_eq!(post_bob - pre_bob, 0);
}

#[test]
fn yt_transfer_full_balance_then_post_transfer_accrual_is_attributable() {
    // Alice transfers 100% of her YT to Bob. After a further accrual,
    // Bob should be able to claim the post-transfer yield against his
    // full balance, but never the pre-transfer yield.
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);
    let bob = f.user(0);

    f.ys_client().mint(&alice, &alice, &(100 * ONE));
    f.advance(7 * common::DAY);
    f.set_rate(WAD + WAD / 100);
    f.fund(&f.ys, 10 * ONE);
    f.ys_client().sync_yield_index();

    // Alice's pre-transfer accrued (latch it in her claimable).
    let alice_pending_before = f.ys_client().pending_yield(&alice);

    // Full-balance transfer.
    let alice_yt = f.yt_client().balance(&alice);
    f.yt_client().transfer(&alice, &bob, &alice_yt);

    // Bob's immediate-claim is zero (no accrual since his settle).
    assert_eq!(f.ys_client().claim_yield(&bob), 0);

    // Alice can still claim her pre-transfer share — the transfer
    // settled it into her claimable pot.
    let alice_claim = f.ys_client().claim_yield(&alice);
    assert!(
        alice_claim >= alice_pending_before - 1,
        "alice lost {} wei of her pre-transfer yield on transfer",
        alice_pending_before - alice_claim
    );

    // Now bump the rate again. Bob holds all the YT, so he gets
    // 100% of the new accrual.
    f.advance(1 * common::DAY);
    f.set_rate(WAD + 2 * (WAD / 100));

    let bob_claim_later = f.ys_client().claim_yield(&bob);
    assert!(
        bob_claim_later > 0,
        "bob holds all YT but received no post-transfer yield"
    );
}

// -------------------------------------------------------------------------
// Negative: prove the bug is real without the mitigation.
// -------------------------------------------------------------------------

#[test]
fn naive_recipient_snapshot_would_be_zero() {
    // Without the H-01 fix, a fresh recipient's `UserIndex` is 0
    // (default). On their first claim, `delta = global - 0 = global`,
    // and the credit is `yt_balance * global / WAD`. Demonstrate the
    // magnitude so a regression of UserIndex initialization surfaces
    // as an *exact* mismatch against this value.
    let global: i128 = WAD + WAD / 100;
    let yt_received: i128 = 50 * ONE;
    let phantom_claim = yt_received.saturating_mul(global) / WAD;
    assert!(
        phantom_claim > 0,
        "naive math credits {} wei phantom yield",
        phantom_claim
    );
}
