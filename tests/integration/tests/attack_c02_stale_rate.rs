//! C-02 — Stale yield index at YT mint regression test.
//!
//! Attack:
//! 1. Bump the Blend rate (without notifying anyone).
//! 2. Mint YT immediately.
//! 3. Claim immediately.
//! 4. If the YS path took the user's index snapshot BEFORE the global
//!    index was advanced to reflect the new Blend rate, the user has
//!    just captured the unaccrued spread.
//!
//! Mitigation: `YS.mint` calls `sync_yield_index` BEFORE invoking
//! `YT.mint`, which in turn invokes `settle_user(to)` via the _update
//! hook. The recipient's snapshot is therefore the post-sync global
//! index. Subsequent claims see `global == snapshot`, returning 0.

use strate_integration_tests::Fixture;
use strate_testutils::attackers::StaleRateYtMintAttacker;
use strate_testutils::AttackResult;

#[test]
fn c02_stale_rate_yt_mint_is_mitigated_e2e() {
    let f = Fixture::default_market();
    let attacker = f.user("alice");

    let result = StaleRateYtMintAttacker::standard().run(&f.env, &f.market, &attacker);
    assert_eq!(
        result,
        AttackResult::Mitigated,
        "C-02 attack succeeded: {result:?}"
    );
}

#[test]
fn c02_two_minters_get_equal_per_yt_yield() {
    // A stronger variant: even if user-B mints AFTER a rate move, they
    // should not receive yield that accrued before they minted.
    use strate_testutils::constants::{ONE_UNDERLYING, WAD};
    use strate_testutils::mock_blend::bumped_rate;

    let f = Fixture::default_market();
    let alice = f.user("alice");
    let bob = f.user("bob");

    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    f.fund(&bob, 1_000 * ONE_UNDERLYING);

    // Alice mints early.
    f.ys().mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));

    // Time passes, rate moves up.
    f.advance_days(30);
    let mock = f.mock_blend();
    mock.set_rate(&bumped_rate(mock.b_rate(), WAD / 100));
    // Refresh the oracle cache so YS.mint can read the new rate.
    let _ = f.oracle().try_sync_rate();

    // Bob mints AFTER the rate move.
    f.ys().mint(&bob, &bob, &(1_000 * ONE_UNDERLYING));

    // No further time; Bob's pending should be ~0.
    let bob_pending = f.ys().pending_yield(&bob);
    assert!(
        bob_pending <= 10,
        "C-02: Bob got pre-mint yield: {bob_pending}"
    );

    // Alice's pending should reflect the rate move on her balance.
    let alice_pending = f.ys().pending_yield(&alice);
    assert!(alice_pending > 0);
}
