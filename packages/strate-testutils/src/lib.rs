//! # `strate-testutils`
//!
//! Test-only utilities for the Strate yield-stripping protocol. Pulls in
//! `soroban-sdk` with `testutils` enabled — this crate must **never** be a
//! runtime dependency of any contract crate; it is `dev-dependencies` only.
//!
//! ## Layout
//!
//! - [`mock_blend`] — a Soroban contract that mimics Blend's lending pool
//!   interface, with a controllable `b_rate` for stale-rate / rate-jump
//!   attack simulation.
//! - [`time`]       — ledger-timestamp manipulation helpers.
//! - [`builders`]   — fluent builders for spinning up a fully-wired market
//!   at a chosen state (TVL, time-to-maturity, user balances).
//! - [`attackers`]  — state-machine harnesses for each known attack class.
//!   Used by both `attacks.rs` integration tests and proptest properties.
//!
//! ## Versioning compatibility
//!
//! This crate matches the Soroban SDK pin in the workspace `Cargo.toml`
//! (22.0.0 at time of writing). When the SDK is upgraded, the testutils
//! must be revalidated; this crate is the canary because it exercises
//! `testutils` features the contracts don't touch in production.
//!
//! ## Threat-model linkage
//!
//! Every attacker harness in [`attackers`] maps 1:1 to an attack ID in
//! `docs/security/THREAT-MODEL.md`. If a new vector is added there, a
//! matching harness must be added here.

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::similar_names)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]

extern crate alloc;

pub mod attackers;
pub mod builders;
#[cfg(feature = "wired")]
pub mod invariants;
pub mod mock_blend;
pub mod time;

/// Result of an attacker harness run.
///
/// Tests assert `AttackResult::Mitigated` on success. If a harness returns
/// `Succeeded`, the protocol contains the vulnerability the harness was
/// built to detect — the test fails loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttackResult {
    /// The contract reverted, or the attack ran to completion but extracted
    /// no excess underlying. This is the desired outcome.
    Mitigated,
    /// The attack extracted underlying beyond the attacker's fair share.
    /// The `extracted_underlying` field is in underlying-native units (i.e.
    /// bUSDC's 7-decimal scale).
    Succeeded {
        /// Net underlying extracted in excess of the attacker's deposit
        /// plus their fair yield share over the holding period.
        extracted_underlying: i128,
    },
}

impl AttackResult {
    /// Convenience for proptest assertions:
    /// `prop_assert!(result.is_mitigated(), "attack succeeded: {:?}", result);`
    #[must_use]
    pub const fn is_mitigated(&self) -> bool {
        matches!(self, Self::Mitigated)
    }

    /// Returns the extracted amount when the attack succeeded; otherwise 0.
    #[must_use]
    pub const fn extracted(&self) -> i128 {
        match self {
            Self::Mitigated => 0,
            Self::Succeeded {
                extracted_underlying,
            } => *extracted_underlying,
        }
    }
}

/// Shared constants used across all harnesses to keep test values stable
/// across reruns.
pub mod constants {
    /// Underlying token has 7 decimals (matches bUSDC on Stellar).
    pub const UNDERLYING_DECIMALS: u32 = 7;

    /// `1.0` in underlying-native units.
    pub const ONE_UNDERLYING: i128 = 10_000_000;

    /// 1.0 in WAD (mirrors `strate_math::WAD`; re-declared here so test
    /// fixtures can be `const`).
    pub const WAD: i128 = 1_000_000_000_000_000_000;

    /// Default tenor for builder-constructed markets: 90 days in seconds.
    pub const DEFAULT_TENOR_SECONDS: u64 = 90 * 86_400;

    /// Window in which the AMM is locked before maturity (matches the
    /// `MARKET_LOCKED_NEAR_MATURITY` constant in the AMM contract).
    pub const MARKET_LOCK_WINDOW_SECONDS: u64 = 30 * 60;

    /// Maximum allowed per-sample Blend `b_rate` delta, in WAD relative.
    /// Default 1% per Oracle sample. Used by harnesses asserting freeze.
    pub const MAX_RATE_DELTA_PER_SAMPLE_WAD: i128 = 10_000_000_000_000_000; // 0.01 WAD
}
