//! Flash-attack mitigation: an adversary manipulates Blend's b_rate
//! (e.g. via a flash-borrowed deposit/withdrawal pattern). The oracle's
//! 5%-per-sync delta cap must freeze rather than propagate the spike.

mod common;

use common::{flatten, OracleFixture, BLEND_ONE};

#[test]
fn flash_doubling_attack_is_mitigated() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // Pre-spike cached rate.
    let pre = client.cached_rate_raw().0;
    assert_eq!(pre, common::WAD);

    // Attacker drives Blend's `b_rate` 2x via flash deposit/withdraw.
    blend.set_rate(&(BLEND_ONE * 200));
    fx.advance_time(1);

    let returned = client.sync_rate();
    assert_eq!(returned, pre, "sync must return last-known-good on rejection");
    assert!(client.is_frozen(), "oracle must be frozen after rejection");

    // The cached rate is unchanged — last-known-good preserved.
    assert_eq!(client.cached_rate_raw().0, pre);
}

#[test]
fn flash_small_jump_blocked_at_five_percent_plus_epsilon() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // Jump by exactly 5.01% — over cap.
    let next = BLEND_ONE + (BLEND_ONE * 501 / 10_000);
    blend.set_rate(&next);
    fx.advance_time(60);

    let _ = client.sync_rate();
    assert!(client.is_frozen(), "5.01% jump must trip the freeze");
}

#[test]
fn slow_attack_cannot_drift_past_cap() {
    // Realistic scenario: attacker has flash capital but must time it
    // across multiple sync calls. Each sync caps at +5%. We confirm
    // that an attempt to push 10% in one sync still fails even if the
    // attacker calls sync multiple times in quick succession (the cap
    // is per-sync, not per-time-window).
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // First +5% — legal.
    blend.set_rate(&(BLEND_ONE + BLEND_ONE * 5 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(!client.is_frozen());

    // Then attempt +10% on top — must freeze.
    let huge = client.cached_rate_raw().0 / common::BLEND_ONE; // back to Blend scale
    blend.set_rate(&(huge + huge * 10 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(client.is_frozen());
}
