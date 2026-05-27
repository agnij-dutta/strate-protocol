//! Basic happy-path: first sync, monotonic re-sync, view reads, admin
//! pause / propose_unpause / execute_unpause.

mod common;

use common::{flatten, OracleFixture, BLEND_ONE, ONE_PERCENT_WAD, TIMELOCK_SECS, WAD};

#[test]
fn first_sync_accepts_any_positive_rate() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();

    // Pre-sync: cached is zero, reads revert.
    let raw = client.cached_rate_raw();
    assert_eq!(raw, (0, 0));

    let rate = client.sync_rate();
    assert_eq!(rate, BLEND_ONE * BLEND_ONE); // 1e9 * 1e9 == 1e18 == WAD

    let (cached, ts) = client.cached_rate_raw();
    assert_eq!(cached, WAD);
    assert!(ts > 0);
}

#[test]
fn get_rate_returns_cached_after_sync() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let _ = client.sync_rate();

    let r = client.get_rate();
    assert_eq!(r, WAD);
}

#[test]
fn second_sync_with_safe_delta_succeeds() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    // Bump Blend by exactly 1% — well under the 5% cap.
    let new_blend_rate = BLEND_ONE + (BLEND_ONE / 100);
    blend.set_rate(&new_blend_rate);
    fx.advance_time(60);

    let after = client.sync_rate();
    assert_eq!(after, new_blend_rate * BLEND_ONE);
}

#[test]
fn admin_pause_blocks_reads_and_syncs() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let _ = client.sync_rate();

    client.pause();
    assert!(client.is_paused());

    // try_get_rate returns Err while paused.
    let r = flatten(client.try_get_rate());
    assert!(r.is_err(), "expected paused read to revert");

    // Propose + execute unpause (48h timelock).
    client.propose_unpause();
    fx.advance_time(TIMELOCK_SECS - 1);
    let early = flatten(client.try_execute_unpause());
    assert!(early.is_err(), "execute_unpause should fail before eta");

    fx.advance_time(2); // now past eta
    client.execute_unpause();
    assert!(!client.is_paused());

    // The 48h advance pushed the cache past the staleness window
    // (max_staleness_ledgers = 120 ledgers ≈ 10 minutes). Resync so
    // the next read sees a fresh cache — in production a keeper does
    // this between the unpause and the next user-facing read.
    let _ = client.sync_rate();

    // Reads work again.
    let r = client.get_rate();
    assert_eq!(r, WAD);
}

#[test]
fn config_view_returns_constructor_values() {
    let fx = OracleFixture::new();
    let cfg = fx.oracle_client().config();
    assert_eq!(cfg.max_delta_per_sync_wad, common::FIVE_PERCENT_WAD);
    assert_eq!(cfg.max_staleness_ledgers, 120);
    assert_eq!(cfg.observation_window, 32);
}

#[test]
fn admin_view_matches_constructor() {
    let fx = OracleFixture::new();
    assert_eq!(fx.oracle_client().admin(), fx.admin);
}

#[test]
fn small_repeated_syncs_below_cap_all_succeed() {
    let fx = OracleFixture::new();
    let client = fx.oracle_client();
    let blend = fx.blend_client();
    let _ = client.sync_rate();

    let mut current = BLEND_ONE;
    for _ in 0..10 {
        // 1% bumps each round, 1% < 5% cap.
        let next = current + (current / 100);
        blend.set_rate(&next);
        fx.advance_time(60);
        let observed = client.sync_rate();
        assert_eq!(observed, next * BLEND_ONE);
        current = next;
    }
    // Suppress an unused-import warning since we don't end up using
    // ONE_PERCENT_WAD here — the comparison is done at the Blend scale.
    let _ = ONE_PERCENT_WAD;
}
