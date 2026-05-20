//! AMM storage layout.
//!
//! Storage policy follows ADR-004 §AMM:
//!
//! | Key                | Type        | Reason                            |
//! |--------------------|-------------|-----------------------------------|
//! | `Config`           | Instance    | Hot config; auto-bumped per call. |
//! | `Paused`           | Instance    | Hot, admin-touched.               |
//! | `Initialized`      | Instance    | Construction marker.              |
//! | `Reserves`         | Persistent  | Read on every swap.               |
//! | `TotalLp`          | Persistent  | Invariant.                        |
//! | `LpBalance(addr)`  | Persistent  | Per-LP.                           |
//! | `LastTradeTime`    | Persistent  | TWAP / rate limiting.             |
//! | `CumulativePrice`  | Persistent  | TWAP accumulator.                 |
//!
//! Per ADR-004, persistent keys are TTL-bumped on every state-changing
//! entrypoint that touches them. The `bump_persistent` helper is the
//! single point that encodes the threshold; changing the threshold is
//! one edit, one PR.

use soroban_sdk::{contracttype, panic_with_error, Address, Env};
use strate_interfaces::Error;

use crate::{PERSISTENT_BUMP_HIGH, PERSISTENT_BUMP_LOW};

/// Immutable per-market AMM config. Set by the constructor once, then
/// only the `fee_bps` field is admin-mutable (within `MAX_FEE_BPS`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct AmmConfig {
    /// PT SAC.
    pub pt: Address,
    /// YT SAC (recorded for symmetry; not traded in v1).
    pub yt: Address,
    /// Underlying SAC (the AMM's quote asset).
    pub underlying: Address,
    /// Strate Oracle.
    pub oracle: Address,
    /// Unix-seconds maturity.
    pub maturity: u64,
    /// Pendle scalar root, WAD-scaled.
    pub scalar_root: i128,
    /// Swap fee in basis points (`0..=MAX_FEE_BPS`).
    pub fee_bps: u32,
    /// Total tenor (maturity - inception) in seconds. Locked at construction.
    pub total_duration: u64,
    /// Admin address (pause / fee adjustment).
    pub admin: Address,
}

/// Storage key enum.
///
/// The shape matches the spec literally — adding a variant must
/// **append**, never insert, so an upgraded contract reading legacy
/// state still maps the right keys.
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    // Instance
    /// Hot config.
    Config,
    /// Pause flag.
    Paused,
    /// Initialised marker (set in `__constructor`).
    Initialized,

    // Persistent
    /// `(x_pt, x_under)` reserve tuple.
    Reserves,
    /// Total LP supply.
    TotalLp,
    /// Per-LP balance.
    LpBalance(Address),
    /// Last swap-or-observation timestamp.
    LastTradeTime,
    /// TWAP cumulative-price accumulator.
    CumulativePrice,
}

// =========================================================================
// Config / instance
// =========================================================================

pub fn write_config(env: &Env, cfg: &AmmConfig) {
    env.storage().instance().set(&DataKey::Config, cfg);
}

pub fn read_config(env: &Env) -> AmmConfig {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

pub fn write_paused(env: &Env, p: bool) {
    env.storage().instance().set(&DataKey::Paused, &p);
}

pub fn read_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}

pub fn mark_initialized(env: &Env) {
    env.storage().instance().set(&DataKey::Initialized, &true);
}

pub fn is_initialized(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Initialized)
        .unwrap_or(false)
}

// =========================================================================
// Persistent
// =========================================================================

pub fn write_reserves(env: &Env, x_pt: i128, x_under: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::Reserves, &(x_pt, x_under));
}

pub fn read_reserves(env: &Env) -> (i128, i128) {
    env.storage()
        .persistent()
        .get(&DataKey::Reserves)
        .unwrap_or((0, 0))
}

pub fn write_total_lp(env: &Env, v: i128) {
    env.storage().persistent().set(&DataKey::TotalLp, &v);
}

pub fn read_total_lp(env: &Env) -> i128 {
    env.storage().persistent().get(&DataKey::TotalLp).unwrap_or(0)
}

pub fn write_lp_balance(env: &Env, who: &Address, v: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::LpBalance(who.clone()), &v);
}

pub fn read_lp_balance(env: &Env, who: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::LpBalance(who.clone()))
        .unwrap_or(0)
}

pub fn write_last_trade_time(env: &Env, t: u64) {
    env.storage()
        .persistent()
        .set(&DataKey::LastTradeTime, &t);
}

pub fn read_last_trade_time(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::LastTradeTime)
        .unwrap_or(0)
}

pub fn write_cumulative_price(env: &Env, v: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::CumulativePrice, &v);
}

pub fn read_cumulative_price(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::CumulativePrice)
        .unwrap_or(0)
}

/// Bump TTL on a single persistent key. Wrapping `extend_ttl` so the
/// thresholds are centralised — see ADR-004.
pub fn bump_persistent(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERSISTENT_BUMP_LOW, PERSISTENT_BUMP_HIGH);
}
