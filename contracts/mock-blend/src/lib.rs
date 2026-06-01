//! Deployable mock Blend pool. Testnet-only stand-in for the real
//! Blend lending pool — returns a settable `b_rate` and a `ReserveData`
//! shape compatible with what the Strate Oracle reads.
//!
//! The production `strate-testutils::MockBlendPool` lives behind the
//! `soroban-sdk/testutils` feature so it cannot be deployed. This crate
//! replicates the deployable surface (constructor + get_reserve_data +
//! set_rate + b_rate) without the testutils dependency, so the testnet
//! oracle has a real contract to call against.
//!
//! ## Surface
//!
//! - `__constructor(underlying, initial_rate)` — bind once
//! - `get_reserve_data() -> ReserveData` — what the oracle reads
//! - `set_rate(new_rate)` — admin-less setter so any caller can advance
//!   the rate. This is **testnet behaviour** by design; the real Blend
//!   pool advances the rate via interest accrual, not direct write
//! - `b_rate() -> i128` — convenience read

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{contract, contractimpl, contracttype, panic_with_error, Address, Env};

/// Storage taxonomy.
#[contracttype]
#[derive(Clone)]
enum DataKey {
    Underlying,
    BRate,
    LastAccrualTs,
    Reserve,
    Initialised,
}

/// Mirrors the subset of Blend's `ReserveData` that strate-oracle reads.
/// Field names + order must match the real Blend pool's surface.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveData {
    pub b_rate: i128,
    pub last_time: u64,
    pub reserve: i128,
}

/// Errors raised by the mock pool.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum MockBlendError {
    AlreadyInitialised = 1,
    InvalidRate = 2,
    NotInitialised = 3,
}

impl From<MockBlendError> for soroban_sdk::Error {
    fn from(e: MockBlendError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

#[contract]
pub struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
    /// One-shot constructor. Sets the underlying token address and the
    /// initial `b_rate`. `initial_rate` is 1e9-scaled to match the real
    /// Blend pool's b_rate format (1_000_000_000 = 1.0).
    pub fn __constructor(env: Env, underlying: Address, initial_rate: i128) {
        let s = env.storage().instance();
        if s.has(&DataKey::Initialised) {
            panic_with_error!(&env, MockBlendError::AlreadyInitialised);
        }
        if initial_rate <= 0 {
            panic_with_error!(&env, MockBlendError::InvalidRate);
        }
        s.set(&DataKey::Underlying, &underlying);
        s.set(&DataKey::BRate, &initial_rate);
        s.set(&DataKey::LastAccrualTs, &env.ledger().timestamp());
        s.set(&DataKey::Reserve, &0_i128);
        s.set(&DataKey::Initialised, &true);
    }

    /// Read the full ReserveData. This is the function strate-oracle
    /// invokes via its `BlendPoolClient`.
    pub fn get_reserve_data(env: Env) -> ReserveData {
        require_init(&env);
        ReserveData {
            b_rate: env.storage().instance().get(&DataKey::BRate).unwrap(),
            last_time: env
                .storage()
                .instance()
                .get(&DataKey::LastAccrualTs)
                .unwrap(),
            reserve: env.storage().instance().get(&DataKey::Reserve).unwrap(),
        }
    }

    /// Bump the rate. Permissionless on testnet so any wallet (or a
    /// scheduled bot) can advance accrual.  Also stamps the accrual
    /// timestamp so the oracle sees a fresh reading.
    pub fn set_rate(env: Env, new_rate: i128) {
        require_init(&env);
        if new_rate <= 0 {
            panic_with_error!(&env, MockBlendError::InvalidRate);
        }
        let s = env.storage().instance();
        s.set(&DataKey::BRate, &new_rate);
        s.set(&DataKey::LastAccrualTs, &env.ledger().timestamp());
    }

    /// Convenience scalar read.
    pub fn b_rate(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::BRate).unwrap()
    }

    /// Convenience underlying read.
    pub fn underlying(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Underlying).unwrap()
    }
}

fn require_init(env: &Env) {
    let init: bool = env
        .storage()
        .instance()
        .get(&DataKey::Initialised)
        .unwrap_or(false);
    if !init {
        panic_with_error!(env, MockBlendError::NotInitialised);
    }
}
