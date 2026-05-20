//! Write 100 observations into a 32-slot ring; verify oldest data is
//! evicted (head wraps correctly) and TWAP reflects only the most
//! recent `window` samples.

mod common;

use common::{OracleFixture, FIVE_PERCENT_WAD, WAD};

#[test]
fn ring_buffer_evicts_oldest_after_overflow() {
    let fx = OracleFixture::with_config(FIVE_PERCENT_WAD, 1_000_000, 32, common::BLEND_ONE);
    let client = fx.oracle_client();

    // Write 100 observations with monotonically-increasing prices.
    // After write #33, the oldest data is being overwritten.
    for i in 1..=100_i128 {
        fx.advance_time(10);
        client.update_observation(&(i * WAD));
    }

    // The buffer now contains samples #69..=#100 (the 32 newest).
    let (newest, _) = client.latest_observation();
    assert_eq!(newest, 100 * WAD);

    // Count saturates at the window.
    assert_eq!(client.observation_count(), 32);

    // TWAP weights the 31 oldest of the 32 remaining (samples #69..=#99)
    // by uniform 10s dts and excludes the newest.
    // TWAP = average of #69..=#99 = (69 + 99) / 2 * WAD = 84 * WAD.
    let twap = client.get_twap();
    let expected = 84 * WAD;
    assert_eq!(twap, expected, "TWAP must reflect the 32 most-recent samples only");
}

#[test]
fn ring_head_returns_to_zero_after_full_revolution() {
    // 4-slot buffer, write exactly 4 then 4 more. Verify wraparound is
    // clean and the newest entry is still the last write.
    let fx = OracleFixture::with_config(FIVE_PERCENT_WAD, 1_000_000, 4, common::BLEND_ONE);
    let client = fx.oracle_client();

    for i in 1..=8_i128 {
        fx.advance_time(10);
        client.update_observation(&(i * WAD));
    }
    let (newest, _) = client.latest_observation();
    assert_eq!(newest, 8 * WAD);
    // TWAP over samples 5..=8 weighted by 10s each (excl. newest 8):
    // (5 + 6 + 7) / 3 = 6 * WAD.
    let twap = client.get_twap();
    assert_eq!(twap, 6 * WAD);
}
