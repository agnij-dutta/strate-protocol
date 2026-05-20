//! TWAP correctness across 8 / 16 / 32-slot ring buffers, including the
//! "buffer not yet full" edge case.

mod common;

use common::{flatten, OracleFixture, FIVE_PERCENT_WAD, WAD};

/// Push `n` observations with prices `[base, base*2, base*3, ...]` and
/// uniform 10-second intervals, then verify the TWAP equals the
/// time-weighted average of all-but-the-newest sample (per the formula
/// in `compute_twap`).
fn twap_uniform_intervals(window: u32) {
    let fx = OracleFixture::with_config(FIVE_PERCENT_WAD, 1_000_000, window, common::BLEND_ONE);
    let client = fx.oracle_client();

    let base = WAD;
    let n_samples: i128 = window as i128;
    for i in 1..=n_samples {
        fx.advance_time(10);
        client.update_observation(&(base * i));
    }

    let twap = client.get_twap();
    // The formula weights each of the n-1 oldest samples by the 10-second
    // forward dt, then divides by (n-1)*10 == sum of dts. The newest is
    // excluded. So TWAP = average of [1*base, 2*base, ..., (n-1)*base]
    //                   = base * n / 2.
    let expected = base * (n_samples) / 2;
    assert_eq!(twap, expected, "TWAP mismatch for window={window}");
}

#[test]
fn twap_window_8() {
    twap_uniform_intervals(8);
}

#[test]
fn twap_window_16() {
    twap_uniform_intervals(16);
}

#[test]
fn twap_window_32() {
    twap_uniform_intervals(32);
}

#[test]
fn twap_with_partial_buffer() {
    // 32-slot buffer but only 3 observations written. The TWAP must
    // still compute over the populated portion.
    let fx = OracleFixture::with_config(FIVE_PERCENT_WAD, 1_000_000, 32, common::BLEND_ONE);
    let client = fx.oracle_client();

    fx.advance_time(10);
    client.update_observation(&WAD);
    fx.advance_time(20);
    client.update_observation(&(2 * WAD));
    fx.advance_time(30);
    client.update_observation(&(3 * WAD));

    let twap = client.get_twap();
    // dts: between obs[0] and obs[1] = 20s; between obs[1] and obs[2] = 30s.
    // TWAP = (1*WAD * 20 + 2*WAD * 30) / 50 = (20 + 60)*WAD / 50 = 80/50 WAD.
    let expected = (20 * WAD + 60 * WAD) / 50;
    assert_eq!(twap, expected);
}

#[test]
fn twap_reverts_with_fewer_than_two_samples() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();

    // Zero samples.
    let r = flatten(client.try_get_twap());
    assert!(r.is_err());

    // One sample — still not enough.
    fx.advance_time(10);
    client.update_observation(&WAD);
    let r = flatten(client.try_get_twap());
    assert!(r.is_err());
}

#[test]
fn latest_observation_returns_newest_after_wrap() {
    // 4-slot buffer, write 6 observations -> head wraps twice.
    let fx = OracleFixture::with_config(FIVE_PERCENT_WAD, 1_000_000, 4, common::BLEND_ONE);
    let client = fx.oracle_client();

    for i in 1..=6_i128 {
        fx.advance_time(10);
        client.update_observation(&(i * WAD));
    }
    let (price, _ts) = client.latest_observation();
    assert_eq!(price, 6 * WAD);
}

#[test]
fn observation_count_caps_at_window() {
    let fx = OracleFixture::with_config(FIVE_PERCENT_WAD, 1_000_000, 4, common::BLEND_ONE);
    let client = fx.oracle_client();

    fx.advance_time(10);
    client.update_observation(&WAD);
    assert_eq!(client.observation_count(), 1);

    for _ in 0..10 {
        fx.advance_time(10);
        client.update_observation(&(2 * WAD));
    }
    assert_eq!(
        client.observation_count(),
        4,
        "observation_count should saturate at window size"
    );
}
