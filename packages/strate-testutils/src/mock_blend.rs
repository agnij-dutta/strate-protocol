//! Mock Blend lending pool.
//!
//! This is the de-risking shim that lets us unit-test Strate without a live
//! Blend deployment. It exposes the **subset** of Blend's actual contract
//! interface that Strate consumes (`b_rate`, `submit`, `get_reserve_data`)
//! plus test-only setters used by attacker harnesses.
//!
//! ## Behavior contract
//!
//! - `exchange_rate` is stored as a 1e9-scaled fixed-point value, matching
//!   the real Blend pool's `b_rate` format.
//! - The rate is **lazy** by default: it does not advance with time unless
//!   either (a) a user-side interaction calls `submit()`, or (b) the test
//!   calls [`MockBlendPoolClient::force_accrue`]. This mimics Blend's
//!   on-chain behaviour: the rate is materialised only at interaction
//!   boundaries.
//! - `set_rate()` is **test-only** and bypasses every invariant Blend's
//!   real pool enforces. Attacker harnesses use this to simulate an
//!   off-protocol rate jump prior to YS observing it.
//! - The pool stores a single underlying-side reserve balance; debt is
//!   simulated implicitly via the accrual delta. We deliberately do not
//!   model borrow positions because Strate never opens one.
//!
//! ## What this mock does NOT simulate
//!
//! - Multiple reserves / oracles inside Blend.
//! - Liquidations.
//! - Blend's emission rewards.
//! - Borrow rate model. (Rate moves only when the test moves it.)
//!
//! ## Threat-model use
//!
//! - **C-02**: attacker harness calls `set_rate(rate * 1.10)` between
//!   YS's `sync_index()` calls, then calls `mint()` and `claim()`.
//! - **A-01**: attacker harness drives a 50%+ rate jump and asserts the
//!   Oracle freezes.

use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, Env};

/// Errors raised by the mock pool.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum MockBlendError {
    /// Rate was set to a non-positive value.
    InvalidRate = 1,
    /// Caller attempted to `submit` without the pool being initialized.
    NotInitialized = 2,
    /// `submit` would have left a negative reserve.
    InsufficientReserve = 3,
}

impl From<MockBlendError> for soroban_sdk::Error {
    fn from(e: MockBlendError) -> Self {
        // Map onto the conventional contract-error channel; tests can match
        // on `Error::from_contract_error(e as u32)`.
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

/// Storage layout keys. Matches Blend's `PoolDataKey` taxonomy at a high
/// level, but only contains the fields Strate observes.
#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// Underlying asset address (SAC handle).
    Underlying,
    /// Current `b_rate`, 1e9-scaled.
    BRate,
    /// Last timestamp at which the rate was materialised.
    LastAccrualTs,
    /// Cumulative reserve underlying balance held by this pool.
    Reserve,
    /// Marker that the pool was initialised.
    Initialised,
}

/// Mirrors the subset of Blend's `ReserveData` Strate reads.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MockReserveData {
    /// `b_rate` 1e9-scaled.
    pub b_rate: i128,
    /// Last accrual timestamp.
    pub last_time: u64,
    /// Underlying held.
    pub reserve: i128,
}

/// The mock contract type. Deploy in tests via
/// `env.register(MockBlendPool, ())`.
#[contract]
pub struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
    /// Initialise the mock pool. Mirrors Blend's `initialize` shape.
    ///
    /// `initial_rate` must be > 0; typical value is `1_000_000_000`
    /// (= 1.0 in 1e9 scaling).
    pub fn initialize(env: Env, underlying: Address, initial_rate: i128) {
        if initial_rate <= 0 {
            panic_with_error!(&env, MockBlendError::InvalidRate);
        }
        env.storage()
            .instance()
            .set(&DataKey::Underlying, &underlying);
        env.storage().instance().set(&DataKey::BRate, &initial_rate);
        env.storage()
            .instance()
            .set(&DataKey::LastAccrualTs, &env.ledger().timestamp());
        env.storage().instance().set(&DataKey::Reserve, &0_i128);
        env.storage().instance().set(&DataKey::Initialised, &true);
    }

    /// **Test-only.** Set the pool's `b_rate` directly.
    ///
    /// Bypasses Blend's accrual; used by attacker harnesses (C-02, A-01).
    /// Production code paths never call this — there is no equivalent on
    /// the real Blend pool.
    pub fn set_rate(env: Env, new_rate: i128) {
        require_init(&env);
        if new_rate <= 0 {
            panic_with_error!(&env, MockBlendError::InvalidRate);
        }
        env.storage().instance().set(&DataKey::BRate, &new_rate);
        // Note: we DO NOT update LastAccrualTs here. That mimics the
        // off-protocol scenario where Blend's rate moves but Strate's
        // Oracle hasn't observed the new value yet.
    }

    /// Force the rate to materialise (mirrors a user calling submit on the
    /// real Blend, which triggers `accrue()` internally). For test-only
    /// rate-advance scenarios where we want the lazy gap to close.
    pub fn force_accrue(env: Env) {
        require_init(&env);
        env.storage()
            .instance()
            .set(&DataKey::LastAccrualTs, &env.ledger().timestamp());
    }

    /// Mimic the field shape Strate's Oracle reads. Returns `b_rate`,
    /// `last_time`, and `reserve`. The real Blend `get_reserve_data`
    /// returns more, but Strate only consumes these three.
    pub fn get_reserve_data(env: Env) -> MockReserveData {
        require_init(&env);
        MockReserveData {
            b_rate: env.storage().instance().get(&DataKey::BRate).unwrap(),
            last_time: env
                .storage()
                .instance()
                .get(&DataKey::LastAccrualTs)
                .unwrap(),
            reserve: env.storage().instance().get(&DataKey::Reserve).unwrap(),
        }
    }

    /// Mimics Blend's `submit` interface for supply/withdraw. We only
    /// model the underlying-side reserve here.
    ///
    /// `delta` is signed: positive = supply, negative = withdraw.
    /// Returns the new `b_rate` so callers can mirror Blend's behaviour of
    /// returning fresh accrual on every interaction.
    pub fn submit(env: Env, from: Address, delta: i128) -> i128 {
        // The real Blend pool meters per-supplier; this mock tracks only
        // the aggregate reserve, so the supplier address is accepted to
        // match the signature but intentionally unused.
        let _ = from;
        require_init(&env);

        let mut reserve: i128 = env.storage().instance().get(&DataKey::Reserve).unwrap();
        let new_reserve = reserve.checked_add(delta).unwrap_or_else(|| {
            panic_with_error!(&env, MockBlendError::InsufficientReserve);
        });
        if new_reserve < 0 {
            panic_with_error!(&env, MockBlendError::InsufficientReserve);
        }
        reserve = new_reserve;
        env.storage().instance().set(&DataKey::Reserve, &reserve);

        // Touching the pool materialises the rate. The real Blend has a
        // more sophisticated model; here we just stamp the timestamp.
        env.storage()
            .instance()
            .set(&DataKey::LastAccrualTs, &env.ledger().timestamp());

        env.storage().instance().get(&DataKey::BRate).unwrap()
    }

    /// Convenience read used by some harnesses.
    pub fn b_rate(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::BRate).unwrap()
    }
}

fn require_init(env: &Env) {
    let init: bool = env
        .storage()
        .instance()
        .get(&DataKey::Initialised)
        .unwrap_or(false);
    if !init {
        panic_with_error!(env, MockBlendError::NotInitialized);
    }
}

// -------------------------------------------------------------------------
// Helper: bump rate by a relative WAD delta. Used by attackers.rs.
// -------------------------------------------------------------------------

/// Compute a new `b_rate` from `old_rate` by a relative WAD delta.
///
/// `rel_delta_wad = 0.01 * WAD` means +1% rate.
/// Saturates at `i128::MAX`; underflow at zero panics.
#[must_use]
pub fn bumped_rate(old_rate: i128, rel_delta_wad: i128) -> i128 {
    // Inline mul-div to avoid pulling strate-math into this trivial helper.
    // `old_rate * (1 + rel_delta_wad) / WAD`. Use i128 widening via
    // saturating arithmetic — the delta in test scenarios is bounded so
    // this is safe.
    let wad: i128 = crate::constants::WAD;
    let one_plus = wad.saturating_add(rel_delta_wad);
    // We deliberately use saturating math here: tests asserting against
    // overflow boundaries use direct `set_rate`, not this helper.
    let raised = (old_rate as i128).saturating_mul(one_plus);
    raised / wad
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{ONE_UNDERLYING, WAD};

    #[test]
    fn bumped_rate_one_percent_matches_expectation() {
        let one_pct = WAD / 100;
        let new_rate = bumped_rate(1_000_000_000, one_pct);
        // 1_000_000_000 * 1.01 = 1_010_000_000
        assert_eq!(new_rate, 1_010_000_000);
    }

    #[test]
    fn bumped_rate_negative_delta_reduces() {
        let neg_one_pct = -(WAD / 100);
        let new_rate = bumped_rate(1_000_000_000, neg_one_pct);
        assert_eq!(new_rate, 990_000_000);
    }

    // Note: contract-level tests live in the consumer crate
    // (`contracts/yield-stripping/tests`), where a Soroban `Env` is
    // available. The unit tests here cover only pure helpers.
    #[test]
    fn one_underlying_constant_sanity() {
        // Catches accidental decimal-shift typos.
        assert_eq!(ONE_UNDERLYING, 10_000_000);
    }
}
