//! End-to-end freeze -> reattest (with timelock) -> unfreeze flow.

mod common;

use common::{flatten, OracleFixture, BLEND_ONE, TIMELOCK_SECS, WAD};

#[test]
fn full_freeze_and_reattest_dance() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();

    // 1. Initial sync (cached = 1.0 WAD).
    let _ = client.sync_rate();

    // 2. Trigger freeze via a 10% rate jump.
    blend.set_rate(&(BLEND_ONE * 110 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(client.is_frozen());

    // 3. Admin proposes reattest at 1.07 WAD (a value the admin
    //    believes reflects truth after off-chain validation).
    let attested = WAD * 107 / 100;
    client.reattest(&attested);

    // 4. Execute too early -> fails.
    fx.advance_time(TIMELOCK_SECS - 10);
    assert!(flatten(client.try_execute_reattest()).is_err());
    assert!(client.is_frozen());

    // 5. After timelock, anyone can execute.
    fx.advance_time(20);
    let new_rate = client.execute_reattest();
    assert_eq!(new_rate, attested);
    assert!(!client.is_frozen());

    // 6. Reads work again.
    assert_eq!(client.get_rate(), attested);
}

#[test]
fn reattest_when_not_frozen_reverts() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let _ = client.sync_rate();
    assert!(!client.is_frozen());

    // Admin shouldn't be able to reattest a healthy oracle — only a
    // frozen one. Catches admin-error / griefing.
    let r = flatten(client.try_reattest(&(WAD * 2)));
    assert!(r.is_err(), "reattest on non-frozen oracle should revert");
}

#[test]
fn cancel_reattest_clears_proposal() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    blend.set_rate(&(BLEND_ONE * 200 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(client.is_frozen());

    let attested = WAD * 110 / 100;
    client.reattest(&attested);
    client.cancel_reattest();

    fx.advance_time(TIMELOCK_SECS + 1);
    let r = flatten(client.try_execute_reattest());
    assert!(
        r.is_err(),
        "execute_reattest must fail with no pending proposal"
    );
    assert!(client.is_frozen(), "cancellation must not unfreeze");
}

#[test]
fn reattest_overwrites_pending_proposal_and_resets_timer() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    blend.set_rate(&(BLEND_ONE * 150 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(client.is_frozen());

    client.reattest(&(WAD * 105 / 100));
    fx.advance_time(TIMELOCK_SECS / 2);
    // Admin observed more truth; revise upward and timer resets.
    client.reattest(&(WAD * 120 / 100));

    fx.advance_time(TIMELOCK_SECS / 2 + 1);
    // We're now at TIMELOCK_SECS + 1 past the FIRST proposal, but only
    // (TIMELOCK_SECS / 2 + 1) past the SECOND — should still be locked.
    let r = flatten(client.try_execute_reattest());
    assert!(
        r.is_err(),
        "second proposal's timer should not yet have elapsed"
    );

    fx.advance_time(TIMELOCK_SECS / 2);
    let r = client.execute_reattest();
    assert_eq!(r, WAD * 120 / 100);
    assert!(!client.is_frozen());
}

#[test]
fn rejected_reattest_with_non_positive_rate() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    blend.set_rate(&(BLEND_ONE * 200 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(client.is_frozen());

    let r = flatten(client.try_reattest(&0));
    assert!(r.is_err());
    let r = flatten(client.try_reattest(&(-1)));
    assert!(r.is_err());
}
