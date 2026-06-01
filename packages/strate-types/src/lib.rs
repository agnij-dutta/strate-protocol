//! Shared constructor argument types for the Strate protocol.
//!
//! These [`contracttype`] structs are the wire format for each contract's
//! `__constructor`. They live here, in a non-cdylib crate, so the Factory
//! can import them without also linking the child contracts' cdylib glue.
//!
//! Why this matters: when crate `A` and crate `B` are both `cdylib +
//! rlib` and crate `F` depends on both for their `rlib` halves, the
//! wasm32 linker tries to merge both `__constructor` symbol exports into
//! `F` and aborts with "Linking globals named `__constructor`: symbol
//! multiply defined". Hoisting the args structs into this crate gives
//! `F` a single source for the types with zero cdylib content attached.
//!
//! Field names + order here are wire-load-bearing. Changing them is a
//! breaking change to every contract's constructor ABI; do it only with
//! a coordinated redeploy of the whole market set.

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{contracttype, Address, String};

// =========================================================================
// PT
// =========================================================================

/// Constructor args for the Principal Token contract.
#[contracttype]
#[derive(Clone)]
pub struct PtInitArgs {
    /// Display handle for the underlying asset (not called into).
    pub underlying: Address,
    /// Maturity timestamp (Unix seconds).
    pub maturity: u64,
    /// Token name, e.g. `"PT-bUSDC-DEC2026"`.
    pub name: String,
    /// Symbol, e.g. `"PT-bUSDC"`.
    pub symbol: String,
    /// Token decimals. Must match the underlying's decimals so 1 PT
    /// redeems 1 underlying at maturity with no scale conversion.
    pub decimals: u32,
}

// =========================================================================
// YT
// =========================================================================

/// Constructor args for the Yield Token contract.
#[contracttype]
#[derive(Clone)]
pub struct YtInitArgs {
    pub underlying: Address,
    pub maturity: u64,
    pub name: String,
    pub symbol: String,
    pub decimals: u32,
}

// =========================================================================
// Oracle
// =========================================================================

/// Constructor args for the Oracle contract.
#[contracttype]
#[derive(Clone, Debug)]
pub struct OracleConfig {
    /// Address of the Blend lending pool we read `b_rate` from.
    pub blend_pool: Address,
    /// Maximum allowed relative jump per `sync_rate`, in WAD.
    /// `5e16` == 5%. A rate moving more than this freezes the oracle.
    pub max_delta_per_sync_wad: i128,
    /// Maximum age (in ledger units) of `CachedRate` before reads revert.
    /// At 5s/ledger, 120 ledgers ≈ 10 minutes.
    pub max_staleness_ledgers: u32,
    /// Size of the TWAP ring buffer. Bounded so it always fits in
    /// instance budget. Values < 2 are rejected at construction.
    pub observation_window: u32,
}

// =========================================================================
// YieldStripping
// =========================================================================

/// Constructor args for the YieldStripping contract. Named `YsConfig`
/// in this crate to avoid colliding with the generic name `Config` when
/// other crates re-export it.
#[contracttype]
#[derive(Clone)]
pub struct YsConfig {
    pub underlying: Address,
    pub pt: Address,
    pub yt: Address,
    pub oracle: Address,
    pub maturity: u64,
    /// AMM curve parameter, immutable per market. YS doesn't use this
    /// directly; recorded for discoverability so external callers can
    /// read the market's curve scaling from a single contract.
    pub scalar_root: i128,
    /// Admin address — single role: pause/unpause.
    pub admin: Address,
}

// =========================================================================
// Factory bundle types
// =========================================================================
//
// Soroban contract entry points are capped at 10 parameters. `deploy_market`
// forwards 13 distinct knobs to the five child contracts, so we bundle the
// oracle config and the PT/YT cosmetic metadata into two contracttype structs.
// The wire format is `Map<Symbol, Val>` keyed by field name, which the SDK
// serialises losslessly across the host boundary.

/// Oracle configuration passed through `Factory::deploy_market` to
/// `Oracle::__constructor`. Same field set as [`OracleConfig`] minus the
/// `blend_pool` which the factory provides separately at deploy time.
#[contracttype]
#[derive(Clone)]
pub struct OracleParams {
    pub max_delta_per_sync_wad: i128,
    pub max_staleness_ledgers: u32,
    pub observation_window: u32,
}

/// PT + YT cosmetic metadata passed through `Factory::deploy_market`. The
/// token decimals must match the underlying asset; PT and YT share the
/// value intentionally so 1 PT + 1 YT redeems 1 U at maturity with no
/// scale conversion.
#[contracttype]
#[derive(Clone)]
pub struct TokenMetadata {
    pub pt_name: String,
    pub pt_symbol: String,
    pub yt_name: String,
    pub yt_symbol: String,
    pub decimals: u32,
}
