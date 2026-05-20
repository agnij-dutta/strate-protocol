//! A-01 — Oracle manipulation via Blend spike.
//!
//! Attack: an adversary controlling some liquidity in the Blend pool
//! flash-borrows + repays in a way that briefly spikes the Blend
//! `b_rate`. If the Oracle accepted that spike, downstream consumers
//! (YS index, AMM TWAP) would be poisoned.
//!
//! Mitigations:
//! - Oracle's `sync_rate` enforces a relative-delta cap (5% per sample).
//! - On violation, the Oracle freezes and returns the **last-known-good**
//!   cached rate; subsequent `get_rate` reverts with `OracleRejected`,
//!   forcing the protocol to wait for admin reattest.

use strate_integration_tests::Fixture;
use strate_testutils::constants::ONE_UNDERLYING;
use strate_testutils::mock_blend::bumped_rate;

#[test]
fn a01_oracle_freezes_on_blend_spike() {
    let f = Fixture::default_market();
    // Establish baseline.
    let _ = f.oracle().sync_rate();
    let (baseline, _) = f.oracle().cached_rate_raw();

    // Simulate a 50% spike from the attacker.
    let mock = f.mock_blend();
    let r0 = mock.b_rate();
    mock.set_rate(&bumped_rate(r0, 500_000_000_000_000_000_i128 /* 50% WAD */));

    // sync_rate freezes silently.
    let _ = f.oracle().try_sync_rate();
    assert!(f.oracle().is_frozen(), "oracle did not freeze on spike");

    // The cached rate did NOT advance to the spiked value.
    let (cached_after, _) = f.oracle().cached_rate_raw();
    assert_eq!(cached_after, baseline, "spike poisoned cache");
}

#[test]
fn a01_below_threshold_jump_is_accepted() {
    // A 4% jump (below the 5% cap) is fine.
    let f = Fixture::default_market();
    let _ = f.oracle().sync_rate();

    let mock = f.mock_blend();
    let r0 = mock.b_rate();
    mock.set_rate(&bumped_rate(r0, 40_000_000_000_000_000_i128 /* 4% WAD */));

    let r = f.oracle().sync_rate();
    assert!(r > 0);
    assert!(!f.oracle().is_frozen(), "oracle wrongly froze on 4% jump");
}

#[test]
fn a01_repeated_max_delta_syncs_advance_smoothly() {
    // 10 consecutive 4.5% bumps should accumulate to ~55%+ without
    // freezing — each individual delta is under the cap.
    let f = Fixture::default_market();
    let _ = f.oracle().sync_rate();

    let mock = f.mock_blend();
    for _ in 0..10 {
        let r0 = mock.b_rate();
        mock.set_rate(&bumped_rate(r0, 45_000_000_000_000_000_i128 /* 4.5% */));
        let _ = f.oracle().sync_rate();
        assert!(!f.oracle().is_frozen());
    }
    let (cached, _) = f.oracle().cached_rate_raw();
    // After 10 x 4.5% compounding ≈ 55%. The cached rate started at WAD.
    assert!(cached > 1_500_000_000_000_000_000_i128); // > 1.5 WAD

    // Mint still works.
    let alice = f.user("alice");
    f.fund(&alice, 1_000 * ONE_UNDERLYING);
    let r = f.ys().try_mint(&alice, &alice, &(1_000 * ONE_UNDERLYING));
    assert!(r.is_ok());
}
