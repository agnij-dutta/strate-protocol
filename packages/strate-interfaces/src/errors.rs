//! Canonical protocol error enum.
//!
//! Every contract in the workspace surfaces errors through this single
//! `Error` enum. The numeric discriminants are stable — adding a new
//! variant must append, never insert, so on-chain consumers (frontend,
//! liquidator bots, indexers) keep working across upgrades.

use soroban_sdk::contracterror;

/// Errors that any Strate contract can return.
///
/// Discriminant policy: append-only. Numbers are reserved in ranges:
///
/// - `1..20`  math
/// - `20..40` access control / lifecycle
/// - `40..60` maturity / state machine
/// - `60..80` AMM / liquidity
/// - `80..100` oracle / pricing
/// - `100..120` token / balance / input validation
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    // --- math (1..20) -----------------------------------------------------
    /// Generic math failure (should rarely surface; specific variants
    /// below are preferred).
    Math = 1,
    /// 256-bit intermediate overflowed.
    MathOverflow = 2,
    /// Result is below the representable range.
    MathUnderflow = 3,
    /// Division by zero.
    DivisionByZero = 4,
    /// `ln` called with `x <= 0`.
    LnOfNegative = 5,

    // --- access / lifecycle (20..40) -------------------------------------
    /// Caller lacks the required role / capability.
    Unauthorized = 20,
    /// Contract is paused.
    Paused = 21,
    /// Contract is already initialized.
    AlreadyInitialized = 22,
    /// Contract has not been initialized.
    NotInitialized = 23,

    // --- maturity / state machine (40..60) -------------------------------
    /// Operation requires maturity to have passed.
    MaturityNotReached = 40,
    /// Operation requires the market to still be pre-maturity.
    MaturityReached = 41,
    /// Liquidity actions are disabled inside the no-trade window
    /// preceding maturity (anti-asymptote guard for the AMM).
    MarketLockedNearMaturity = 42,
    /// Maturity supplied at construction is invalid (in the past or
    /// outside the configured policy window).
    InvalidMaturity = 43,

    // --- AMM / liquidity (60..80) ----------------------------------------
    /// Not enough liquidity in the pool to satisfy this trade.
    InsufficientLiquidity = 60,
    /// Quoted output is below the caller's minimum tolerance.
    SlippageExceeded = 61,
    /// Deposit is below the per-market minimum.
    MinDepositNotMet = 62,

    // --- oracle (80..100) ------------------------------------------------
    /// Oracle observation is older than the staleness threshold.
    OracleStale = 80,
    /// Oracle observation is implausible (sanity checks rejected it).
    OracleRejected = 81,

    // --- token / input validation (100..120) -----------------------------
    /// Caller's balance is below the requested amount.
    InsufficientBalance = 100,
    /// Underlying asset is not one the protocol supports for this
    /// factory.
    InvalidUnderlying = 101,
}
