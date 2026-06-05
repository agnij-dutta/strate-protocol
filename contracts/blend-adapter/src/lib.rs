//! Blend V2 → V1 reserve-data shim.
//!
//! Strate's Oracle was written against the V1 Blend ABI:
//!
//!   `Blend::get_reserve_data() -> ReserveData { b_rate, last_time, reserve }`
//!
//! Blend V2 (mainnet) exposes a different shape:
//!
//!   `Blend::get_reserve(asset: Address) -> Reserve { data: ReserveData, ... }`
//!
//! and stores `b_rate` in 1e12 fixed point instead of 1e9.
//!
//! This adapter is the translation layer. One instance per (pool, asset)
//! pair. The Oracle binds to the adapter address as if it were a V1
//! pool; the adapter forwards reads to V2 and re-scales the rate.
//!
//! ## Surface
//!
//! - `__constructor(blend_pool, asset)` — bind once
//! - `get_reserve_data() -> ReserveData` — what the Oracle reads
//! - `blend_pool() -> Address` — convenience read
//! - `asset() -> Address` — convenience read
//!
//! ## Why an adapter instead of patching the Oracle
//!
//! The Oracle is part of the v0.1.0-h01-fix audit baseline. Touching it
//! invalidates the diff under review. The adapter is a small, isolated
//! contract whose entire job is one cross-contract call plus a divide
//! by 1000 — straightforward to verify by inspection.

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, panic_with_error, Address, Env,
};

/// ReserveData shape Strate's Oracle expects. `b_rate` is 1e9-scaled.
///
/// Must match `oracle::ReserveData` in `strate-oracle/src/lib.rs` exactly
/// (field names, order, types). Soroban contracttype serialisation is
/// positional + named, so drift here breaks the Oracle silently.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveData {
    /// `b_rate`, 1e9-scaled (V1 convention).
    pub b_rate: i128,
    /// Last accrual timestamp.
    pub last_time: u64,
    /// Underlying reserve held by the pool (unused by the Oracle —
    /// passed through so the struct shape stays identical).
    pub reserve: i128,
}

/// Blend V2's ReserveData. `b_rate` is 1e12-scaled. We mirror only the
/// fields needed for the translation; the actual V2 struct has more.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlendV2ReserveData {
    pub d_rate: i128,
    pub b_rate: i128,
    pub ir_mod: i128,
    pub b_supply: i128,
    pub d_supply: i128,
    pub backstop_credit: i128,
    pub last_time: u64,
}

/// Blend V2's Reserve struct (wraps ReserveData + the asset address).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlendV2Reserve {
    pub asset: Address,
    pub config: BlendV2ReserveConfig,
    pub data: BlendV2ReserveData,
    pub scalar: i128,
}

/// Subset of V2 ReserveConfig — we only need to round-trip the struct.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlendV2ReserveConfig {
    pub index: u32,
    pub decimals: u32,
    pub c_factor: u32,
    pub l_factor: u32,
    pub util: u32,
    pub max_util: u32,
    pub r_base: u32,
    pub r_one: u32,
    pub r_two: u32,
    pub r_three: u32,
    pub reactivity: u32,
    pub supply_cap: i128,
    pub enabled: bool,
}

#[contractclient(name = "BlendV2PoolClient")]
pub trait BlendV2Pool {
    fn get_reserve(env: Env, asset: Address) -> BlendV2Reserve;
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    BlendPool,
    Asset,
    Initialised,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AdapterError {
    AlreadyInitialised = 1,
    NotInitialised = 2,
    /// b_rate read from Blend was zero or negative. Surfaces a clear
    /// error so the Oracle's "rate must be positive" guard reports
    /// against the upstream symptom, not a scaling artefact.
    InvalidUpstreamRate = 3,
}

impl From<AdapterError> for soroban_sdk::Error {
    fn from(e: AdapterError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

/// V1 → V2 rate scale factor. Blend V2 uses 1e12, V1 used 1e9, so we
/// divide by 1000 to land in V1 space.
const RATE_SCALE_V2_TO_V1: i128 = 1_000;

#[contract]
pub struct BlendAdapter;

#[contractimpl]
impl BlendAdapter {
    pub fn __constructor(env: Env, blend_pool: Address, asset: Address) {
        let s = env.storage().instance();
        if s.has(&DataKey::Initialised) {
            panic_with_error!(&env, AdapterError::AlreadyInitialised);
        }
        s.set(&DataKey::BlendPool, &blend_pool);
        s.set(&DataKey::Asset, &asset);
        s.set(&DataKey::Initialised, &true);
    }

    /// What the Oracle reads. Forwards to Blend V2 and rescales the
    /// rate. Panics if Blend returns a non-positive rate so the
    /// Oracle's own checks see a clean error code rather than silently
    /// dividing.
    pub fn get_reserve_data(env: Env) -> ReserveData {
        require_init(&env);
        let blend_pool: Address = env.storage().instance().get(&DataKey::BlendPool).unwrap();
        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();

        let v2 = BlendV2PoolClient::new(&env, &blend_pool).get_reserve(&asset);
        if v2.data.b_rate <= 0 {
            panic_with_error!(&env, AdapterError::InvalidUpstreamRate);
        }
        ReserveData {
            b_rate: v2.data.b_rate / RATE_SCALE_V2_TO_V1,
            last_time: v2.data.last_time,
            reserve: v2.data.b_supply,
        }
    }

    pub fn blend_pool(env: Env) -> Address {
        env.storage().instance().get(&DataKey::BlendPool).unwrap()
    }

    pub fn asset(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Asset).unwrap()
    }
}

fn require_init(env: &Env) {
    let init: bool = env
        .storage()
        .instance()
        .get(&DataKey::Initialised)
        .unwrap_or(false);
    if !init {
        panic_with_error!(env, AdapterError::NotInitialised);
    }
}
