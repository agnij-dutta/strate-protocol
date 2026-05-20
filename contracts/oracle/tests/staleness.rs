//! Staleness gating on `get_rate`. Reads must revert with
//! `OracleStale` after `max_staleness_ledgers * SECONDS_PER_LEDGER`.

mod common;

use common::{flatten, OracleFixture, SECS_PER_LEDGER};

#[test]
fn fresh_read_succeeds() {
    let fx = OracleFixture::tight(); // 10-ledger staleness
    let client = fx.oracle_client();
    let _ = client.sync_rate();
    let _ = client.get_rate(); // immediate read: not stale
}

#[test]
fn stale_read_reverts() {
    let fx = OracleFixture::tight();
    let client = fx.oracle_client();
    let _ = client.sync_rate();

    // tight() = 10 ledgers staleness = 50 seconds.
    fx.advance_time(10 * SECS_PER_LEDGER + 1);

    let r = flatten(client.try_get_rate());
    assert!(
        r.is_err(),
        "get_rate must revert with OracleStale past the staleness window"
    );
}

#[test]
fn pre_sync_get_rate_reverts() {
    let fx = OracleFixture::tight();
    let client = fx.oracle_client();
    // Never synced.
    let r = flatten(client.try_get_rate());
    assert!(r.is_err(), "get_rate must revert before any sync");
}

#[test]
fn resync_resets_staleness() {
    let fx = OracleFixture::tight();
    let client = fx.oracle_client();
    let _ = client.sync_rate();

    fx.advance_time(10 * SECS_PER_LEDGER + 1);
    let r = flatten(client.try_get_rate());
    assert!(r.is_err());

    // Re-sync (Blend rate unchanged): cached_rate unchanged but cached_at
    // updated. Subsequent read succeeds.
    let _ = client.sync_rate();
    let _ = client.get_rate(); // success
}
