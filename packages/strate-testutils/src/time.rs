//! Ledger-time manipulation helpers for tests.
//!
//! Soroban's `LedgerInfo` controls `timestamp`, `sequence_number`, and a
//! few protocol fields. Property tests routinely need to "fast-forward to
//! one second before maturity", "advance by N days while keeping the
//! sequence linear", or "set the clock back" (only valid in tests; the
//! contracts must monotonically check `now`). These helpers wrap the
//! mechanical `env.ledger().with_mut` boilerplate.

use soroban_sdk::{testutils::Ledger as _, Env};

/// 1 second.
pub const SECOND: u64 = 1;
/// 1 minute in seconds.
pub const MINUTE: u64 = 60;
/// 1 hour in seconds.
pub const HOUR: u64 = 60 * MINUTE;
/// 1 day in seconds.
pub const DAY: u64 = 24 * HOUR;
/// 1 week in seconds.
pub const WEEK: u64 = 7 * DAY;
/// Approximate year, 365 days.
pub const YEAR: u64 = 365 * DAY;

/// Read the current ledger timestamp.
#[must_use]
pub fn now(env: &Env) -> u64 {
    env.ledger().timestamp()
}

/// Set the ledger timestamp to an absolute value. Bumps the sequence
/// number by 1 — tests routinely interleave `advance_to` and writes, and
/// keeping the sequence monotone avoids "stale TTL" warnings on the host.
///
/// Panics in tests if `new_ts < current_ts` to catch accidental clock
/// rewinds. If a test legitimately needs to go backwards, call
/// [`force_set`].
pub fn advance_to(env: &Env, new_ts: u64) {
    let current = env.ledger().timestamp();
    assert!(
        new_ts >= current,
        "advance_to called with {new_ts} but ledger is already at {current}; use force_set"
    );
    env.ledger().with_mut(|li| {
        li.timestamp = new_ts;
        li.sequence_number = li.sequence_number.saturating_add(1);
    });
}

/// Advance the ledger by a delta in seconds.
pub fn advance_by(env: &Env, delta: u64) {
    let target = env.ledger().timestamp().saturating_add(delta);
    advance_to(env, target);
}

/// Force the timestamp to an arbitrary value, including backwards. Tests
/// that exercise "this should revert because `now > maturity`" don't
/// need this; tests that simulate a chain reorg or replay attack do.
pub fn force_set(env: &Env, new_ts: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp = new_ts;
        li.sequence_number = li.sequence_number.saturating_add(1);
    });
}

/// Position the ledger exactly at the AMM lock boundary
/// (`maturity - MARKET_LOCK_WINDOW_SECONDS`) — useful in C-03 fuzzing.
///
/// Returns the timestamp that was set.
pub fn advance_to_amm_lock_boundary(env: &Env, maturity: u64) -> u64 {
    let target = maturity.saturating_sub(crate::constants::MARKET_LOCK_WINDOW_SECONDS);
    advance_to(env, target);
    target
}

/// Position the ledger one second before maturity. Useful for the "swap
/// must revert in the lock window" assertion.
pub fn advance_to_just_before_maturity(env: &Env, maturity: u64) -> u64 {
    let target = maturity.saturating_sub(1);
    advance_to(env, target);
    target
}

/// Position the ledger exactly at maturity. Post-maturity claim / redeem
/// flows kick in.
pub fn advance_to_maturity(env: &Env, maturity: u64) -> u64 {
    advance_to(env, maturity);
    maturity
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn advance_by_is_monotone() {
        let env = Env::default();
        env.ledger().with_mut(|li| li.timestamp = 1_000);
        advance_by(&env, 500);
        assert_eq!(env.ledger().timestamp(), 1_500);
    }

    #[test]
    #[should_panic(expected = "use force_set")]
    fn advance_to_backwards_panics() {
        let env = Env::default();
        env.ledger().with_mut(|li| li.timestamp = 1_000);
        advance_to(&env, 999);
    }

    #[test]
    fn force_set_can_go_backwards() {
        let env = Env::default();
        env.ledger().with_mut(|li| li.timestamp = 1_000);
        force_set(&env, 500);
        assert_eq!(env.ledger().timestamp(), 500);
    }

    #[test]
    fn lock_boundary_helpers_align_with_constant() {
        let env = Env::default();
        let maturity = 100_000_u64;
        let boundary = advance_to_amm_lock_boundary(&env, maturity);
        assert_eq!(
            boundary,
            maturity - crate::constants::MARKET_LOCK_WINDOW_SECONDS
        );
    }
}
