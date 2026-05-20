//! Verify that a Blend rate jump > `max_delta_per_sync_wad` freezes the
//! oracle, that subsequent reads revert, and that a sub-threshold jump
//! does NOT freeze.

mod common;

use common::{flatten, OracleFixture, BLEND_ONE};

#[test]
fn five_percent_jump_freezes_oracle() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // Bump Blend by 6% — over the default 5% cap.
    let bumped = BLEND_ONE + (BLEND_ONE * 6 / 100);
    blend.set_rate(&bumped);
    fx.advance_time(60);

    // sync_rate returns last-good (Ok) but sets the freeze flag.
    let returned = client.sync_rate();
    assert_eq!(
        returned, common::WAD,
        "sync_rate must return last-known-good on violation"
    );
    assert!(client.is_frozen(), "oracle should be frozen after over-cap jump");
}

#[test]
fn monotonicity_violation_freezes_oracle() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // Try to lower Blend's reported rate — must freeze (monotonicity).
    let lower = BLEND_ONE - 1;
    blend.set_rate(&lower);
    fx.advance_time(60);

    let returned = client.sync_rate();
    assert_eq!(returned, common::WAD, "monotonicity violation must preserve cached rate");
    assert!(client.is_frozen());
}

#[test]
fn after_freeze_reads_revert() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    blend.set_rate(&(BLEND_ONE * 110 / 100));
    fx.advance_time(60);
    let _ = client.sync_rate();
    assert!(client.is_frozen());

    let r = flatten(client.try_get_rate());
    assert!(r.is_err(), "get_rate must revert while frozen");

    let r = flatten(client.try_update_observation(&common::WAD));
    assert!(r.is_err(), "update_observation must revert while frozen");
}

#[test]
fn jump_just_under_cap_does_not_freeze() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // 4.99% bump — under the 5% cap.
    let bumped = BLEND_ONE + (BLEND_ONE * 499 / 10_000);
    blend.set_rate(&bumped);
    fx.advance_time(60);

    let result = client.sync_rate();
    assert_eq!(result, bumped * BLEND_ONE);
    assert!(!client.is_frozen());
}

#[test]
fn first_sync_with_extreme_initial_rate_succeeds() {
    // First sync has no `cached_rate > 0`, so the delta cap is bypassed
    // by design. The check exists from sync #2 onward.
    let fx = OracleFixture::with_config(
        common::FIVE_PERCENT_WAD,
        120,
        8,
        BLEND_ONE * 1_000, // 1000x the canonical 1.0 — extreme but legal as init
    );
    let client = fx.oracle_client();
    let r = client.sync_rate();
    assert_eq!(r, BLEND_ONE * 1_000 * BLEND_ONE);
}
