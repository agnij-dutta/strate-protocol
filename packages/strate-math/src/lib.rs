//! # `strate-math`
//!
//! Fixed-point math for the Strate yield-stripping protocol on Stellar
//! Soroban.
//!
//! All values are 18-decimal fixed-point ("WAD"), stored in `i128`. We use
//! signed integers because the AMM's `lnImpliedRate` and balance deltas
//! span both signs, and because Pendle's reference implementation does.
//!
//! ## Layout
//!
//! - [`fixed`] — WAD multiply/divide with explicit rounding direction.
//! - [`exp`]   — `exp_wad(x)` via 2^k * exp(r) range reduction.
//! - [`ln`]    — `ln_wad(x)` via k*ln(2) + ln(r) range reduction.
//! - [`rounding`] — explicit [`rounding::RoundingMode`] enum to force every
//!   call site to declare its rounding direction (Pendle's anti-drift rule).
//!
//! ## Overflow safety
//!
//! Every public function documents its overflow behavior. Internally, the
//! intermediate products are computed at 256-bit width using a manual
//! `(hi, lo)` split of the `i128` operands, then reduced back to `i128`
//! with checked arithmetic. There is no `unsafe` and no implicit panics in
//! library code; out-of-range inputs return [`Error`] from a `_checked`
//! variant, while the non-checked wrappers panic with a deterministic
//! message (these are only used inside contracts that have already
//! validated inputs at the protocol boundary).
//!
//! ## `no_std`
//!
//! This crate is `no_std`-compatible. Tests link to the host allocator via
//! the dev-dependency on `proptest`, but the library itself uses only
//! `core::`.

#![no_std]
#![deny(missing_docs)]
#![deny(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::similar_names)]
#![allow(clippy::unreadable_literal)]

pub mod exp;
pub mod fixed;
pub mod ln;
pub mod rounding;

pub use exp::exp_wad;
pub use fixed::{
    div_wad, div_wad_down, div_wad_up, mul_wad, mul_wad_down, mul_wad_up, muldiv_ceil,
    muldiv_checked, muldiv_floor,
};
pub use ln::ln_wad;
pub use rounding::RoundingMode;

// -------------------------------------------------------------------------
// Constants
// -------------------------------------------------------------------------

/// The fixed-point unit (1.0 in WAD): `1e18`.
pub const WAD: i128 = 1_000_000_000_000_000_000;

/// Half of [`WAD`] — used for banker's rounding tie-break.
pub const HALF_WAD: i128 = WAD / 2;

/// `ln(2)` in WAD. Truncated to 18 decimals from
/// `0.69314718055994530941723212145817656807550013436025...`.
pub const LN2: i128 = 693_147_180_559_945_309;

/// `ln(2) / 2` in WAD, used by `exp_wad` range reduction.
pub const LN2_HALF: i128 = 346_573_590_279_972_654;

/// Minimum representable fixed value (matches `i128::MIN`).
pub const MIN_FIXED: i128 = i128::MIN;

/// Maximum representable fixed value (matches `i128::MAX`).
pub const MAX_FIXED: i128 = i128::MAX;

// -------------------------------------------------------------------------
// Error
// -------------------------------------------------------------------------

/// Errors returned by math primitives.
///
/// These are intentionally narrow: protocol-level errors (auth, pause,
/// maturity) live in `strate-interfaces::errors`. Contracts convert
/// [`Error`] into their protocol error at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Multiplication or addition overflowed the 256-bit intermediate.
    Overflow,
    /// Result is below the representable range.
    Underflow,
    /// Divisor was zero.
    DivisionByZero,
    /// `ln_wad` was called on `x <= 0`.
    LnOfNonPositive,
    /// `exp_wad` input was outside the supported domain.
    ExpOutOfDomain,
}
